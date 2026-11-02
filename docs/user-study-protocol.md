# N6-3 New User Study Protocol

## Objective

Validate that developers unfamiliar with bpm-engine can complete the two
north-star scenarios from an empty environment, and collect data to improve
documentation and interface design.

## North-Star Scenarios

### Scenario 1: Enterprise Approval Agent

A developer defines, deploys, and runs a process that:
1. Retrieves materials (external task)
2. Generates a structured recommendation (model call with JSON Schema output)
3. Requires human approval before executing a side-effect tool
4. Compensates on failure

**Steps**:
1. Clone the repo, build the engine (`cargo build`)
2. Start the engine (`cargo run -p bpm-engine-server-rest`)
3. Deploy a flow (via `POST /deploy` with BPMN XML)
4. Validate before deploy (`POST /validate`)
5. Start an instance (`POST /process-instances`)
6. Run the ai-worker template (`cd templates/ai-worker && cargo run`)
7. Observe the instance reach the approval step
8. Approve via `POST /approvals/:id/decide`
9. Observe the instance complete

### Scenario 2: Event-Driven Agent

A developer runs a process that waits for an external message:
1. Deploy a flow with a message catch node
2. Start an instance (it waits at the catch)
3. Deliver a message via `POST /process-instances/:id/messages`
4. Observe the instance consume the message and proceed
5. Broadcast a signal via `POST /signals`

## Study Protocol

### Participants
- At least 3 developers who have NOT contributed to bpm-engine
- Familiarity with Rust and REST APIs (target audience)
- No prior exposure to the engine's API or docs

### Materials Provided
- The repository (empty environment, no pre-existing data)
- The 15-minute quickstart (`docs/quickstart.md`)
- The agent operation recipes (`docs/agent-recipes.md`)
- The troubleshooting guide (`docs/troubleshooting.md`)
- The production worker template (`templates/production-worker-README.md`)

### Metrics Collected
For each participant and scenario:

| Metric | How to Measure |
|--------|---------------|
| Time to first instance started | Wall clock from clone to `POST /process-instances` 201 |
| Time to scenario complete | Wall clock from clone to instance `COMPLETED` |
| First-validation pass rate | Did `POST /validate` return `valid: true` on first attempt? |
| Blockers encountered | Notes on where the participant got stuck |
| Documentation gaps | What did they look for and not find? |
| Error recovery | How did they recover from errors (retry, docs, ask)? |

### Procedure
1. **Setup** (5 min): Participant clones the repo, reads the quickstart
2. **Scenario 1** (30 min): Enterprise Approval Agent
   - Facilitator records time checkpoints and blockers
   - Participant works autonomously (facilitator observes, doesn't guide)
3. **Break** (5 min): Participant notes initial impressions
4. **Scenario 2** (20 min): Event-Driven Agent
5. **Debrief** (10 min):
   - What was confusing?
   - What documentation was missing?
   - What would they change about the API?
   - Would they use this engine for a real project?

### Success Criteria
- ≥2 of 3 participants complete both scenarios within the time budget
- First-validation pass rate ≥50% (participants deploy valid definitions
  without iteration on at least half of attempts)
- No blocker requires facilitator intervention (self-service docs suffice)

### Data Governance
- Participant identities are anonymized in the report
- No code or API keys are shared outside the study
- Feedback is recorded as structured notes (blockers, times, suggestions)

## Study Execution Status

**HUMAN_BLOCKED**: The study requires at least 3 human participants who have
not contributed to bpm-engine. This cannot be executed autonomously. The
protocol, materials, and metrics framework are delivered (this document);
the study execution is pending human participation.

## Follow-Up Actions (Post-Study)

Based on the study results:
1. Adjust documentation where participants got stuck
2. Simplify API responses that caused confusion
3. Add examples for workflows participants attempted but couldn't complete
4. Decide next-round feature scope based on participant requests
5. Re-run the study after improvements (iterative)

## Reference Materials

- [15-minute quickstart](quickstart.md)
- [Agent operation recipes](agent-recipes.md)
- [Troubleshooting](troubleshooting.md)
- [Production worker template](../templates/production-worker-README.md)
- [MCP server guide](mcp-server.md)
- [DSL guide](dsl-guide.md)
