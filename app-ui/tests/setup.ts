const g = globalThis as unknown as {
  jsdom?: { window: { localStorage: Storage; sessionStorage: Storage } };
  localStorage?: Storage;
  sessionStorage?: Storage;
};

if (g.jsdom) {
  for (const key of ['localStorage', 'sessionStorage'] as const) {
    if (!g[key]) {
      Object.defineProperty(globalThis, key, {
        value: g.jsdom.window[key],
        configurable: true,
        writable: true,
      });
    }
  }
}

if (typeof globalThis.window !== 'undefined' && !globalThis.PointerEvent) {
  Object.defineProperty(globalThis, 'PointerEvent', {
    value: globalThis.window.MouseEvent,
    configurable: true,
    writable: true,
  });
}
