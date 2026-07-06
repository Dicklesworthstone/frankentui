// FrankenTermJS first-party React adapter — also the Next.js wiring
// (bd-2vr05.9.3). Lifecycle contract: mount -> attach -> resize/input ->
// detach -> dispose, driven from a single effect. StrictMode double-invokes
// the effect in development; every step below is idempotent-or-deduped, so
// the double run is harmless by construction.
// Generated in lockstep with ftui-web's sdk_adapter model; do not hand-edit.
"use client";

import { useEffect, useRef } from "react";

export function FrankenTerm({ FrankenTermWeb, transportUrl, onEvent }) {
  const containerRef = useRef(null);

  useEffect(() => {
    // Next.js SSR guard: engine code is browser-only.
    if (typeof window === "undefined" || !containerRef.current) {
      return undefined;
    }
    const container = containerRef.current;

    // Step 1: pin the contract before any other call.
    const contract = FrankenTermWeb.apiContract();
    if (contract.apiLine !== "frankenterm-web" || !String(contract.apiVersion).startsWith("1.")) {
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
        onEvent?.(JSON.parse(line));
      }
    }, 16);

    // Effect cleanup IS the teardown: detach, then destroy. StrictMode runs
    // mount+cleanup twice in development; the adapter model dedups repeats.
    return () => {
      clearInterval(drainTimer);
      container.removeEventListener("keydown", onKeyDown);
      resizeObserver.disconnect();
      term.attachClose(); // Detach
      term.destroy(); // Dispose
    };
  }, [FrankenTermWeb, transportUrl, onEvent]);

  return <div ref={containerRef} style={{ width: "100%", height: "100%" }} />;
}
