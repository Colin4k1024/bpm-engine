# BPM Operations Console

The React console exposes the engine's operational surface instead of only a read-only trace. It includes:

- runtime overview, liveness, readiness, and invariant checks;
- BPMN deployment, definition versions, activation, and deprecation;
- process instance launch, inventory, variables, topology, history, and replay;
- human-task forms and completion;
- external-task fetch-and-lock, completion, failure, and lease extension;
- dead-letter inspection, requeue, and deletion;
- optional tenant and API-key headers stored in the browser.

## Run locally

Start the REST server from the repository root:

```bash
cargo run -p bpm-engine-server-rest
```

Then start the console:

```bash
cd ui
npm install
npm run dev
```

Open `http://localhost:5173`. Vite proxies `/api` to `http://localhost:3000`.

## Verify

```bash
npm run lint
npm run build
```

The production output is written to `ui/dist/`.
