import { test, expect } from '@playwright/test'

const screenshotDir = '/Users/jiafan/.codex/visualizations/2026/07/22/019f89cd-4a69-7100-a0ed-e6458d48bf46'

const approvalBpmn = `<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL" targetNamespace="http://bpm.local/e2e">
  <process id="e2e-approval:1" isExecutable="true">
    <startEvent id="start" />
    <userTask id="review" name="Review request" />
    <endEvent id="done" />
    <sequenceFlow id="flow-1" sourceRef="start" targetRef="review" />
    <sequenceFlow id="flow-2" sourceRef="review" targetRef="done" />
  </process>
</definitions>`

let traceInstanceId

async function apiPost(request, path, data) {
  const response = await request.post(`/api/v1${path}`, { data })
  expect(response.ok(), `${path}: ${response.status()} ${await response.text()}`).toBeTruthy()
  return response.json()
}

async function capture(page, name) {
  await page.screenshot({ path: `${screenshotDir}/${name}.png`, fullPage: true })
}

test.describe.serial('BPM operations console pages', () => {
  test.beforeAll(async ({ request }) => {
    const health = await request.get('/health')
    expect(health.ok()).toBeTruthy()

    const deploy = await request.post('/api/v1/process-definitions/deploy', {
      data: approvalBpmn,
      headers: { 'Content-Type': 'application/xml' },
    })
    expect(deploy.ok(), await deploy.text()).toBeTruthy()

    await apiPost(request, '/process-instances', { process_def_id: 'e2e-approval:1', variables: { owner: 'playwright' } })

    const trace = await apiPost(request, '/process-instances', { process_def_id: 'minimal', variables: { suite: 'pages' } })
    traceInstanceId = trace.instance_id

    await apiPost(request, '/process-instances', { process_def_id: 'payment-flow', variables: { purpose: 'dead-letter' } })
    let failedTaskId
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const tasks = await apiPost(request, '/external-tasks/fetch-and-lock', {
        worker_id: 'playwright-dlq', task_types: ['payment'], max_tasks: 1, lock_duration_ms: 30_000,
      })
      expect(tasks).toHaveLength(1)
      failedTaskId = tasks[0].task_id
      await apiPost(request, `/external-tasks/${encodeURIComponent(failedTaskId)}/fail`, {
        worker_id: 'playwright-dlq', error: `Playwright failure ${attempt + 1}`,
      })
    }

    await apiPost(request, '/process-instances', { process_def_id: 'payment-flow', variables: { purpose: 'worker-lab' } })
  })

  test('overview page', async ({ page }) => {
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Workflow control plane' })).toBeVisible()
    await expect(page.getByText('Runtime readiness')).toBeVisible()
    await expect(page.getByText(/\d+ violations/)).toBeVisible()
    await capture(page, '01-overview')
  })

  test('definitions page', async ({ page }) => {
    await page.goto('/definitions')
    await expect(page.getByRole('heading', { name: 'Process definitions' })).toBeVisible()
    await page.getByRole('button', { name: /e2e-approval/ }).first().click()
    await expect(page.getByText('Graph model')).toBeVisible()
    await expect(page.getByText('review', { exact: true })).toBeVisible()
    await capture(page, '02-definitions')
  })

  test('instances page', async ({ page }) => {
    await page.goto('/instances')
    await expect(page.getByRole('heading', { name: 'Process instances' })).toBeVisible()
    await expect(page.getByRole('cell', { name: 'e2e-approval:1' }).first()).toBeVisible()
    await expect(page.getByRole('link', { name: /Open trace/ }).first()).toBeVisible()
    await capture(page, '03-instances')
  })

  test('human tasks page and completion', async ({ page }) => {
    await page.goto('/tasks')
    await expect(page.getByRole('heading', { name: 'Task inbox' })).toBeVisible()
    await page.getByRole('button', { name: /review/ }).first().click()
    await expect(page.getByRole('heading', { name: 'Complete review' })).toBeVisible()
    await capture(page, '04-human-tasks')
    await page.getByRole('button', { name: 'Complete task' }).click()
    await expect(page.getByText(/completed/)).toBeVisible()
  })

  test('external worker lab', async ({ page }) => {
    await page.goto('/workers')
    await expect(page.getByRole('heading', { name: 'External worker lab' })).toBeVisible()
    await page.getByLabel('Worker ID').fill('playwright-worker')
    await page.getByLabel('Topics').fill('payment')
    await page.getByRole('button', { name: 'Fetch and lock tasks' }).click()
    await expect(page.getByText(/Locked 1 task/)).toBeVisible()
    await expect(page.getByRole('heading', { name: 'payment' })).toBeVisible()
    await page.getByRole('button', { name: 'Extend lease' }).click()
    await expect(page.getByText(/lock extended/)).toBeVisible()
    await capture(page, '05-worker-lab')
  })

  test('dead letters page', async ({ page }) => {
    await page.goto('/dead-letters')
    await expect(page.getByRole('heading', { name: 'Dead letters' })).toBeVisible()
    await page.getByRole('button', { name: /Playwright failure 3/ }).first().click()
    await expect(page.getByText('Playwright failure 3').last()).toBeVisible()
    await expect(page.getByRole('button', { name: 'Requeue task' })).toBeVisible()
    await capture(page, '06-dead-letters')
  })

  test('diagnostics page', async ({ page }) => {
    await page.goto('/diagnostics')
    await expect(page.getByRole('heading', { name: 'Diagnostics' })).toBeVisible()
    await expect(page.getByText('instance_version_positive').first()).toBeVisible()
    await expect(page.getByText('database')).toBeVisible()
    await capture(page, '07-diagnostics')
  })

  test('trace and replay page', async ({ page }) => {
    await page.goto(`/trace/${traceInstanceId}`)
    await expect(page.getByText('Process topology')).toBeVisible()
    await expect(page.getByText('Execution history')).toBeVisible()
    await page.getByRole('button', { name: 'Create replay' }).click()
    await expect(page.getByText(/0 \/ 4/)).toBeVisible()
    await page.getByRole('button', { name: 'Step forward' }).click()
    await expect(page.getByText(/1 \/ 4/)).toBeVisible()
    await capture(page, '08-trace-replay')
  })

  test('Chinese localization across all pages and persisted language switch', async ({ page }) => {
    await page.goto('/')
    await page.getByLabel('Language').selectOption('zh')
    await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN')
    await expect(page.getByRole('heading', { name: '工作流控制台' })).toBeVisible()
    await capture(page, 'zh-01-overview')

    await page.reload()
    await expect(page.getByRole('heading', { name: '工作流控制台' })).toBeVisible()

    const pages = [
      ['/definitions', '流程定义', 'zh-02-definitions'],
      ['/instances', '流程实例', 'zh-03-instances'],
      ['/tasks', '任务收件箱', 'zh-04-human-tasks'],
      ['/workers', '外部 Worker 实验室', 'zh-05-worker-lab'],
      ['/dead-letters', '死信队列', 'zh-06-dead-letters'],
      ['/diagnostics', '运行诊断', 'zh-07-diagnostics'],
    ]
    for (const [path, heading, screenshot] of pages) {
      await page.goto(path)
      await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible()
      await capture(page, screenshot)
    }

    await page.goto(`/trace/${traceInstanceId}`)
    await expect(page.getByText('流程拓扑')).toBeVisible()
    await expect(page.getByText('执行历史')).toBeVisible()
    await capture(page, 'zh-08-trace')

    await page.goto('/')
    await page.getByLabel('语言').selectOption('en')
    await expect(page.getByRole('heading', { name: 'Workflow control plane' })).toBeVisible()
  })
})
