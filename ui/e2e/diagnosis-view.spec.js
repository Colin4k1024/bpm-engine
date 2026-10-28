import { test, expect } from '@playwright/test'

// E6-2 (issue #27): the AI diagnosis view. Fully driven through the real
// REST API — no mocks: one payment-flow instance is failed three times
// (the first attempt records a full AI task envelope with model, tools,
// cost and an artifact; the third exhausts the retries and lands in the
// dead-letter queue), one payment instance keeps a live lease (in-flight
// attempt), and one approval instance waits on its user task.

const approvalBpmn = `<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" targetNamespace="http://bpm.local/e2e">
  <process id="e2e-diagnosis:1" isExecutable="true">
    <startEvent id="start" />
    <userTask id="review" name="Review request" />
    <endEvent id="done" />
    <sequenceFlow id="flow-1" sourceRef="start" targetRef="review" />
    <sequenceFlow id="flow-2" sourceRef="review" targetRef="done" />
  </process>
</definitions>`

let failedInstanceId
let inFlightInstanceId
let approvalInstanceId

async function apiPost(request, path, data) {
  const response = await request.post(`/api/v1${path}`, { data })
  expect(response.ok(), `${path}: ${response.status()} ${await response.text()}`).toBeTruthy()
  return response.json()
}

// Lock the payment task belonging to ONE instance. fetch-and-lock is
// topic-scoped, not instance-scoped, and tasks left ready by earlier runs
// (e.g. an expired lease) can be returned too — fetch a wide window and
// pick the instance's own task, waiting briefly for strays to age out.
async function lockForInstance(request, workerId, instanceId) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    const tasks = await apiPost(request, '/external-tasks/fetch-and-lock', {
      worker_id: workerId, task_types: ['payment'], max_tasks: 10, lock_duration_ms: 30_000,
    })
    const mine = tasks.find((task) => task.process_instance_id === instanceId)
    if (mine) return mine
    await new Promise((resolve) => setTimeout(resolve, 1_000))
  }
  throw new Error(`no available payment task for instance ${instanceId}`)
}

test.describe.serial('AI diagnosis view (E6-2)', () => {
  test.beforeAll(async ({ request }) => {
    const health = await request.get('/health')
    expect(health.ok()).toBeTruthy()

    const deploy = await request.post('/api/v1/process-definitions/deploy', {
      data: approvalBpmn,
      headers: { 'Content-Type': 'application/xml' },
    })
    expect(deploy.ok(), await deploy.text()).toBeTruthy()

    const approval = await apiPost(request, '/process-instances', {
      process_def_id: 'e2e-diagnosis:1', variables: { owner: 'playwright-diagnosis' },
    })
    approvalInstanceId = approval.instance_id

    // Attempt 1: fails WITH an AI task envelope (E3-1) recorded in the
    // ExternalTaskFailed history event — model, tools, cost, artifact.
    const failed = await apiPost(request, '/process-instances', {
      process_def_id: 'payment-flow', variables: { purpose: 'diagnosis-e2e' },
    })
    failedInstanceId = failed.instance_id
    const task = await lockForInstance(request, 'diag-worker-a', failedInstanceId)
    await apiPost(request, `/external-tasks/${encodeURIComponent(task.task_id)}/fail`, {
      worker_id: 'diag-worker-a',
      error: 'Diagnosis e2e: model over quota (attempt 1)',
      envelope: {
        schema_version: 1,
        execution_id: `${task.task_id}:0`,
        retry_category: 'transient',
        model: { provider: 'openai', name: 'gpt-4o-mini', version: '2024-07-18' },
        tools: [{ name: 'web_search', version: '2.1.0' }],
        cost: { currency: 'USD', amount_micros: 1500, tokens_in: 1204, tokens_out: 37, duration_ms: 980 },
        artifacts: [{
          uri: 'https://storage.example.com/diagnosis/report.md',
          sha256: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
          size: 4096,
          media_type: 'text/markdown',
        }],
      },
    })
    // Attempts 2 and 3: plain failures; the third exhausts the retries
    // (payment-flow retries: 3) → dead letter + TokenFailed.
    const second = await lockForInstance(request, 'diag-worker-b', failedInstanceId)
    await apiPost(request, `/external-tasks/${encodeURIComponent(second.task_id)}/fail`, {
      worker_id: 'diag-worker-b', error: 'Diagnosis e2e: degraded dependency (attempt 2)',
    })
    const third = await lockForInstance(request, 'diag-worker-c', failedInstanceId)
    await apiPost(request, `/external-tasks/${encodeURIComponent(third.task_id)}/fail`, {
      worker_id: 'diag-worker-c', error: 'Diagnosis e2e: retries exhausted (attempt 3)',
    })

    // In-flight instance: a lease is taken and never resolved.
    const inFlight = await apiPost(request, '/process-instances', {
      process_def_id: 'payment-flow', variables: { purpose: 'diagnosis-in-flight' },
    })
    inFlightInstanceId = inFlight.instance_id
    await lockForInstance(request, 'diag-worker-hold', inFlightInstanceId)
  })

  test('renders steps, lease attempts, errors and AI envelope metadata', async ({ page }) => {
    await page.goto(`/diagnosis/${failedInstanceId}`)
    await expect(page.getByRole('heading', { name: 'Step diagnosis' })).toBeVisible()
    await expect(page.getByText('payment-flow').first()).toBeVisible()

    // Steps: the plain start node and the external-task step.
    await expect(page.getByText('start', { exact: true })).toBeVisible()
    await expect(page.getByText('payment', { exact: true })).toBeVisible()
    await expect(page.locator('.badge-external-task')).toHaveCount(1)

    // Three lease attempts, all failed, one per worker.
    await expect(page.locator('.attempt')).toHaveCount(3)
    await expect(page.locator('.attempt-failed')).toHaveCount(3)
    await expect(page.getByText('diag-worker-a')).toBeVisible()
    await expect(page.getByText('diag-worker-b')).toBeVisible()
    await expect(page.getByText('diag-worker-c')).toBeVisible()
    await expect(page.locator('.attempt-failed').first().getByText(/retries left/)).toBeVisible()

    // Recorded error text (monospace, full text on hover via title).
    await expect(page.locator('.attempt-error').first()).toHaveText(/model over quota \(attempt 1\)/)

    // The attempt-1 AI envelope: model, tools, cost, artifact reference.
    await expect(page.getByText('openai/gpt-4o-mini (2024-07-18)')).toBeVisible()
    await expect(page.getByText('web_search@2.1.0')).toBeVisible()
    await expect(page.getByText('USD 0.0015')).toBeVisible()
    await expect(page.getByText(/tokens 1204 in \/ 37 out/)).toBeVisible()
    await expect(page.getByText('https://storage.example.com/diagnosis/report.md')).toBeVisible()
    await expect(page.getByRole('button', { name: 'Copy' })).toBeVisible()

    // The events toggle expands the payment step's causal event list.
    const paymentStep = page.locator('.step-item', { hasText: 'payment' })
    await paymentStep.getByRole('button', { name: /\d+ events/ }).click()
    await expect(paymentStep.locator('.step-events')).toContainText('TokenArrived')
    await expect(paymentStep.locator('.step-events')).toContainText('ExternalTaskLocked')
    await expect(paymentStep.locator('.step-events')).toContainText('TokenFailed')

    // Recovery jumps are offered on the terminal failure.
    await expect(page.getByRole('link', { name: 'View in history →' }).first()).toBeVisible()
    await expect(page.getByRole('link', { name: 'View dead letters →' }).first()).toBeVisible()
  })

  test('error → history jump applies the token and event-type filters', async ({ page }) => {
    await page.goto(`/diagnosis/${failedInstanceId}`)
    await page.getByRole('link', { name: 'View in history →' }).first().click()
    await page.waitForURL(/\/trace\/[^/]+\?token_id=.+&event_type=ExternalTaskFailed/)

    // The history view is filtered to the failing step's external-task
    // failures (three attempts).
    await expect(page.getByText('Execution history')).toBeVisible()
    await expect(page.locator('.timeline-item')).toHaveCount(3)
    await expect(page.locator('.timeline-item').first()).toContainText('ExternalTaskFailed')

    // The trace page links back into the diagnosis view.
    await page.getByRole('link', { name: 'AI diagnosis' }).click()
    await expect(page).toHaveURL(new RegExp(`/diagnosis/${failedInstanceId}`))
    await expect(page.getByRole('heading', { name: 'Step diagnosis' })).toBeVisible()
  })

  test('in-flight attempt shows the lease-held state', async ({ page }) => {
    await page.goto(`/diagnosis/${inFlightInstanceId}`)
    await expect(page.getByRole('heading', { name: 'Step diagnosis' })).toBeVisible()
    await expect(page.locator('.attempt')).toHaveCount(1)
    await expect(page.locator('.attempt-in_flight')).toHaveCount(1)
    await expect(page.getByText('diag-worker-hold')).toBeVisible()
    await expect(page.getByText(/lease held, awaiting outcome/)).toBeVisible()
  })

  test('approval-required indication on a waiting user task', async ({ page }) => {
    await page.goto(`/diagnosis/${approvalInstanceId}`)
    await expect(page.getByRole('heading', { name: 'Step diagnosis' })).toBeVisible()
    await expect(page.getByText('review', { exact: true })).toBeVisible()
    await expect(page.locator('.badge-approval-required')).toHaveCount(1)
  })

  test('dead letters deep link filters the queue to the instance', async ({ page }) => {
    await page.goto(`/diagnosis/${failedInstanceId}`)
    await page.getByRole('link', { name: 'View dead letters →' }).first().click()
    await page.waitForURL(new RegExp(`/dead-letters\\?instance_id=${failedInstanceId}`))

    await expect(page.getByRole('heading', { name: 'Dead letters' })).toBeVisible()
    // Only this instance's exhausted task is listed, not the rest of the
    // queue (the engine keeps entries from other runs).
    await expect(page.locator('.record-list > button')).toHaveCount(1)
    await expect(page.locator('.record-list > button').first()).toContainText('retries exhausted (attempt 3)')

    // Clearing the filter restores the full queue.
    await page.getByRole('button', { name: 'Clear filter' }).click()
    await expect(page).toHaveURL(/\/dead-letters$/)
    await expect(page.locator('.record-list > button').first()).toBeVisible()
  })

  test('Chinese localization of the diagnosis view', async ({ page }) => {
    await page.goto(`/diagnosis/${failedInstanceId}`)
    await page.getByLabel('Language').selectOption('zh')
    await expect(page.getByRole('heading', { name: '步骤诊断', exact: true })).toBeVisible()
    await expect(page.getByText('外部任务', { exact: true })).toBeVisible()
    await expect(page.getByRole('link', { name: '在历史中查看 →' }).first()).toBeVisible()
    await expect(page.getByRole('link', { name: '查看死信队列 →' }).first()).toBeVisible()
    await page.getByLabel('语言').selectOption('en')
    await expect(page.getByRole('heading', { name: 'Step diagnosis' })).toBeVisible()
  })
})
