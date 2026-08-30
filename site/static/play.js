(() => {
  const KEYS_THE_GAME_OWNS = new Set([
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "KeyQ",
    "KeyE",
    "Space",
  ]);

  const STATE_LABELS = {
    loading: "Loading the verified browser build\u2026",
    ready: "Ready. Click the canvas, then use the arrow keys, Q, E, and Space.",
    error: "This build failed to initialize. See the captured detail below.",
  };

  const errorSink = document.getElementById("browser-errors");
  const canvas = document.getElementById("game-canvas");
  const stateLabel = document.querySelector("[data-state-label]");

  const sanitize = (value) =>
    String(value ?? "")
      .replaceAll(window.location.origin, "")
      .replace(/[\u0000-\u001f\u007f]+/g, " ")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 200);

  const record = (message) => {
    if (!errorSink) return;
    const detail = sanitize(message) || "unknown browser failure";
    errorSink.hidden = false;
    errorSink.textContent = errorSink.textContent
      ? `${errorSink.textContent}\n${detail}`
      : detail;
  };

  window.addEventListener("error", (event) => {
    record(event.message || event.error || "error event");
  });

  window.addEventListener("unhandledrejection", (event) => {
    record(event.reason ? event.reason.message || event.reason : "unhandled rejection");
  });

  const setState = (state) => {
    document.body.dataset.gameState = state;
    if (stateLabel) stateLabel.textContent = STATE_LABELS[state] ?? state;
  };

  const canvasIsFocused = () => canvas !== null && document.activeElement === canvas;

  for (const type of ["keydown", "keyup"]) {
    window.addEventListener(
      type,
      (event) => {
        if (canvasIsFocused() && KEYS_THE_GAME_OWNS.has(event.code)) {
          event.preventDefault();
        }
      },
      { passive: false },
    );
  }

  if (canvas) {
    canvas.addEventListener("pointerdown", () => canvas.focus({ preventScroll: true }));
    canvas.addEventListener("contextmenu", (event) => event.preventDefault());
  }

  if (stateLabel) {
    new MutationObserver(() => {
      const state = document.body.dataset.gameState ?? "loading";
      stateLabel.textContent = STATE_LABELS[state] ?? state;
    }).observe(document.body, {
      attributeFilter: ["data-game-state"],
    });
  }

  setState("loading");

  import("./game.js")
    .then((module) => module.default())
    .catch((failure) => {
      setState("error");
      record(failure && failure.message ? failure.message : failure);
    });
})();
