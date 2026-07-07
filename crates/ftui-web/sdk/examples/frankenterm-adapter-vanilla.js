// FrankenTermJS first-party vanilla adapter (bd-2vr05.9.3).
// Lifecycle contract: mount -> attach -> resize/input -> detach -> dispose.
// Generated in lockstep with ftui-web's sdk_adapter model; do not hand-edit.

export function createFrankenTermAdapter(FrankenTermWeb, container, transportUrl) {
  // Step 1: pin the contract before any other call.
  const contract = FrankenTermWeb.apiContract();
  if (contract.apiLine !== "frankenterm-js" || !String(contract.apiVersion).startsWith("1.")) {
    throw new Error(`unsupported FrankenTermWeb contract: ${contract.apiVersion}`);
  }

  // Step 2 (Mount): initialize the engine into the container.
  const term = FrankenTermWeb.init(container);

  // Step 3: size to the container now, then keep sizing on changes.
  term.fitToContainer();
  const resizeObserver = new ResizeObserver(() => term.fitToContainer());
  resizeObserver.observe(container);

  // Step 4 (Attach): connect the transport only after init succeeded.
  term.attachConnect(transportUrl);

  // Step 5: forward host input (legal only while attached).
  const onKeyDown = (domEvent) => term.input(domEvent);
  container.addEventListener("keydown", onKeyDown);

  // Step 6: drain typed events on the host's schedule (drain-driven).
  const drainTimer = setInterval(() => {
    for (const line of term.drainEventSubscriptionJsonl()) {
      handleTerminalEvent(JSON.parse(line));
    }
  }, 16);

  // Teardown order matters: detach the transport, then destroy the engine.
  // Vanilla hosts must call dispose() exactly once.
  return {
    term,
    dispose() {
      clearInterval(drainTimer);
      container.removeEventListener("keydown", onKeyDown);
      resizeObserver.disconnect();
      term.attachClose(); // Detach
      term.destroy(); // Dispose
    },
  };
}

function handleTerminalEvent(event) {
  // Route by the typed taxonomy from sdk/frankenterm-js-events.d.ts.
  console.debug("frankenterm event", event.type, event);
}
