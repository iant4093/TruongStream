// @vitest-environment jsdom
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { App } from '../src/App';
import { WindowControls } from '../src/components/WindowControls';
import { installBrowserPreviewMocksIfNeeded } from '../src/dev/mockTauri';
import { useStreamStore } from '../src/state/useStreamStore';

const { winState, mockWindow } = vi.hoisted(() => {
  const winState = {
    maximized: false,
    fullscreen: false,
  };
  const mockWindow = {
    close: vi.fn(async () => {}),
    minimize: vi.fn(async () => {}),
    isMaximized: vi.fn(async () => winState.maximized),
    toggleMaximize: vi.fn(async () => {
      winState.maximized = !winState.maximized;
    }),
    isFullscreen: vi.fn(async () => winState.fullscreen),
    setFullscreen: vi.fn(async (next: boolean) => {
      winState.fullscreen = next;
    }),
  };
  return { winState, mockWindow };
});

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => mockWindow,
}));

const initialState = useStreamStore.getState();

beforeAll(() => {
  vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
    cb(0);
    return 1;
  });
  vi.stubGlobal('cancelAnimationFrame', () => {});
  installBrowserPreviewMocksIfNeeded();
});

afterEach(async () => {
  cleanup();
  winState.maximized = false;
  winState.fullscreen = false;
  vi.clearAllMocks();
  await useStreamStore.getState().disconnectCamera();
  useStreamStore.setState(initialState, true);
});

describe('TruongStream Studio UI', () => {
  it('discovers Canon EOS R7, connects, streams telemetry, and updates camera properties', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/Canon EOS R7 \(480 Mbps\)/)).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^Connect$/i }));
    });

    await waitFor(() => {
      expect(screen.getByText(/59\.9 fps/)).toBeTruthy();
      expect(screen.getByText(/RF-S 18-150mm F3\.5-6\.3 IS STM/)).toBeTruthy();
      expect(screen.getByText('M (Manual)')).toBeTruthy();
      expect(screen.getAllByText('ISO 800').length).toBeGreaterThan(0);
      expect(screen.getAllByText('1/125').length).toBeGreaterThan(0);
      expect(screen.getAllByText('Whole Area AF').length).toBeGreaterThan(0);
    });

    // Change AF Area from Whole Area AF to 1-Point AF
    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'AF Area' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: '1-Point AF' }));
    });
    await waitFor(() => {
      expect(screen.getAllByText('1-Point AF').length).toBeGreaterThan(0);
    });

    // Open ISO Sensitivity dropdown and change from ISO 800 to ISO 1600
    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'ISO Sensitivity' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: 'ISO 1600' }));
    });

    await waitFor(() => {
      expect(screen.getAllByText('ISO 1600').length).toBeGreaterThan(0);
    });

    // Switch Focus Mode between One-Shot and AF-C from the live stage bar
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'AF-C' }));
    });
    await waitFor(() => {
      expect(screen.getAllByText('AF-C (Servo AF)').length).toBeGreaterThan(0);
    });

    // Click viewport to trigger autofocus reticle
    const stage = screen.getByRole('button', { name: /Live View canvas/i });
    vi.spyOn(stage, 'getBoundingClientRect').mockReturnValue({
      left: 0,
      top: 0,
      width: 1000,
      height: 562.5,
      right: 1000,
      bottom: 562.5,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    await act(async () => {
      fireEvent.click(stage, { clientX: 500, clientY: 280 });
    });

    expect(document.querySelector('.focus-reticle')).toBeTruthy();
    await waitFor(() => {
      expect(screen.getAllByText('1-Point AF').length).toBeGreaterThan(0);
    });

    // Clicking the pinned reticle unlocks point focus and restores Whole Area AF
    await act(async () => {
      fireEvent.click(
        screen.getByRole('button', { name: 'Unlock focus point and return to Whole Area AF' }),
      );
    });
    expect(document.querySelector('.focus-reticle')).toBeNull();
    await waitFor(() => {
      expect(screen.getAllByText('Whole Area AF').length).toBeGreaterThan(0);
    });

    // Click stage again and unlock via the toolbar Whole Area AF button
    await act(async () => {
      fireEvent.click(stage, { clientX: 600, clientY: 300 });
    });
    expect(document.querySelector('.focus-reticle')).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Whole Area AF' }));
    });
    expect(document.querySelector('.focus-reticle')).toBeNull();
    await waitFor(() => {
      expect(screen.getAllByText('Whole Area AF').length).toBeGreaterThan(0);
    });
  });

  it('switches to Color & LUT and System tabs and updates LUT, on-canvas split handle, and resolution', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/Canon EOS R7/)).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^Connect$/i }));
    });

    const stage = screen.getByRole('button', { name: /Live View canvas/i });
    vi.spyOn(stage, 'getBoundingClientRect').mockReturnValue({
      left: 0,
      top: 0,
      width: 1000,
      height: 562.5,
      right: 1000,
      bottom: 562.5,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    // Toggle LUT split-screen wipe & thirds grid
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Thirds Grid/i }));
      fireEvent.click(screen.getByRole('button', { name: /LUT Split/i }));
    });
    expect(document.querySelector('.rule-of-thirds')).toBeTruthy();
    const splitHandle = await waitFor(() =>
      screen.getByRole('slider', { name: 'LUT split wipe position' }),
    );
    expect(splitHandle.classList.contains('stage-split-handle')).toBe(true);
    expect(document.querySelector('.stage-toolbar input[type="range"]')).toBeNull();

    // Drag the on-canvas split handle and verify position updates without triggering AF reticle
    await act(async () => {
      fireEvent.pointerDown(splitHandle, { pointerId: 1, clientX: 500 });
      fireEvent.pointerMove(splitHandle, { pointerId: 1, clientX: 720 });
      fireEvent.pointerUp(splitHandle, { pointerId: 1, clientX: 720 });
    });
    expect(splitHandle.getAttribute('aria-valuenow')).toBe('72');
    expect(document.querySelector('.focus-reticle')).toBeNull();

    // Switch to Color & LUT inspector tab
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Color & LUT' }));
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'Built-in LUT preset' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: 'Cinematic Teal & Orange' }));
    });

    await act(async () => {
      fireEvent.change(screen.getByRole('slider', { name: 'LUT Intensity' }), {
        target: { value: '75' },
      });
    });
    expect(screen.getByText('75%')).toBeTruthy();

    // Switch to System tab and verify Native / 4K / 30 FPS + Auto-Sleep controls
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'System' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Native' }));
    });
    await waitFor(() => {
      expect(screen.getAllByText(/960×640/).length).toBeGreaterThan(0);
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '4K UHD' }));
      fireEvent.click(screen.getByRole('button', { name: '30 FPS' }));
    });

    await waitFor(() => {
      expect(screen.getAllByText(/3840×2160/).length).toBeGreaterThan(0);
      expect(screen.getByText(/30\.0 fps/)).toBeTruthy();
      expect(screen.getByText('IOSurface #248')).toBeTruthy();
      expect(
        (screen.getByLabelText('Local MJPEG stream URL') as HTMLInputElement).value,
      ).toBe('http://127.0.0.1:49210/stream.mjpeg');
      expect(screen.getByRole('button', { name: /Check for Updates/i })).toBeTruthy();
    });
  });

  it('automatically updates Shutter Speed for the 180-degree shutter rule when FPS Cap changes', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/Canon EOS R7/)).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^Connect$/i }));
    });

    await waitFor(() => {
      expect(screen.getByRole('combobox', { name: 'Shutter Speed' })).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'Shutter Speed' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: 'Auto (180° Rule · 1/125)' }));
    });

    await waitFor(() => {
      expect(screen.getByText('Auto 180° · 1/125')).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'System' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: '30 FPS' }));
    });

    await waitFor(() => {
      expect(screen.getByText(/30\.0 fps/)).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Camera' }));
    });

    await waitFor(() => {
      expect(screen.getByText('Auto 180° · 1/60')).toBeTruthy();
    });
  });

  it('locks exposure controls and warns when camera dial is not in Manual (M) mode', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/Canon EOS R7/)).toBeTruthy();
    });

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^Connect$/i }));
    });

    const currentCaps = useStreamStore.getState().capabilities;
    const currentTelemetry = useStreamStore.getState().telemetry;
    expect(currentCaps).toBeTruthy();

    await act(async () => {
      useStreamStore.setState({
        pollFrame: async () => {},
        telemetry: currentTelemetry
          ? {
              ...currentTelemetry,
              streaming: false,
            }
          : null,
        capabilities: currentCaps
          ? {
              ...currentCaps,
              exposure_mode: 'Av (Aperture Priority)',
              manual_mode_active: false,
            }
          : null,
      });
    });

    await waitFor(() => {
      expect(screen.getByText(/Manual Mode \(M\) Required/i)).toBeTruthy();
      expect(screen.getByText(/Set camera dial to M \(Manual\)/i)).toBeTruthy();
    });

    expect((screen.getByRole('combobox', { name: 'Shutter Speed' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('combobox', { name: 'Aperture' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('combobox', { name: 'ISO Sensitivity' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('combobox', { name: 'Exposure Comp (EV)' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('combobox', { name: 'AF Area' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('exposes all 10 LUT presets, custom context menu, Clean Output Mode, and hoverable traffic lights', async () => {
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/Canon EOS R7 \(480 Mbps\)/)).toBeTruthy();
    });
    expect(screen.queryByText(/PTP Sim/)).toBeNull();

    // Window controls render hoverable macOS glyph icons + Toggle fullscreen button
    const controls = document.querySelector('.window-controls');
    expect(controls?.querySelectorAll('.traffic-light svg').length).toBe(3);
    expect(screen.getByRole('button', { name: 'Toggle fullscreen' })).toBeTruthy();

    // Color & LUT tab offers all 10 prepackaged LUT presets
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Color & LUT' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'Built-in LUT preset' }));
    });
    expect(screen.getAllByRole('option').length).toBe(10);
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: 'Fujifilm Eterna Cinema' }));
    });

    // Right-click opens TruongStream custom context menu & toggles Clean Output Mode
    await act(async () => {
      fireEvent.contextMenu(document.querySelector('.app-window-frame')!, {
        clientX: 200,
        clientY: 200,
      });
    });
    const menu = screen.getByRole('menu', { name: 'TruongStream context menu' });
    expect(menu).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByRole('menuitem', { name: /Clean Output Mode/i }));
    });
    expect(document.querySelector('.app-window-frame.is-clean-output')).toBeTruthy();
    expect(document.querySelector('.workspace-titlebar')).toBeNull();

    // Escape exits Clean Output Mode cleanly
    await act(async () => {
      fireEvent.keyDown(window, { key: 'Escape' });
    });
    expect(document.querySelector('.app-window-frame.is-clean-output')).toBeNull();
    expect(document.querySelector('.workspace-titlebar')).toBeTruthy();

    // Connect Canon EOS R7 and verify Hybrid Uncompressed 1080p/4K HDMI input pairing
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^Connect$/i }));
    });
    await act(async () => {
      useStreamStore.setState((s) => ({
        cameras: [
          ...s.cameras,
          {
            id: 'hdmi:camlink-4k',
            brand: 'hdmi',
            vendor_id: 0x0fd9,
            product_id: 0x0066,
            model_name: 'Cam Link 4K (HDMI 2160p)',
            serial_number: 'camlink-4k',
            usb_speed_mbps: 5000,
          },
        ],
      }));
      fireEvent.click(screen.getByRole('button', { name: 'System' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('combobox', { name: 'Hybrid HDMI Video Input' }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('option', { name: 'Cam Link 4K (HDMI 2160p)' }));
    });
    await waitFor(() => {
      expect(screen.getByText(/Uncompressed HDMI -> 1080p \(1920×1080 input\)/)).toBeTruthy();
      expect(screen.getByText(/1920×1080 HDMI/)).toBeTruthy();
    });
  });

  it('renders native Windows caption controls (minimize, maximize/restore, fullscreen, close) and handles Windows layout + F11', async () => {
    const platformSpy = vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('Win32');
    try {
      render(<App />);

      await waitFor(() => {
        expect(document.querySelector('.workspace-titlebar.is-windows')).toBeTruthy();
      });

      // Windows places caption controls on the far right inside .topbar-right, without macOS traffic lights
      expect(document.querySelector('.window-controls')).toBeNull();
      const winControls = document.querySelector('.topbar-right .window-controls-win');
      expect(winControls).toBeTruthy();

      const minimizeBtn = screen.getByRole('button', { name: 'Minimize window' });
      const maxBtn = screen.getByRole('button', { name: 'Maximize window' });
      const fullBtn = screen.getByRole('button', { name: 'Toggle fullscreen' });
      const closeBtn = screen.getByRole('button', { name: 'Close window' });

      await act(async () => {
        fireEvent.click(minimizeBtn);
      });
      expect(mockWindow.minimize).toHaveBeenCalledTimes(1);

      await act(async () => {
        fireEvent.click(maxBtn);
      });
      expect(mockWindow.toggleMaximize).toHaveBeenCalledTimes(1);
      await waitFor(() => {
        expect(screen.getByRole('button', { name: 'Restore window' })).toBeTruthy();
      });

      // Restoring via double-clicking titlebar header also toggles maximize state
      await act(async () => {
        fireEvent.doubleClick(document.querySelector('.workspace-titlebar')!);
      });
      expect(mockWindow.toggleMaximize).toHaveBeenCalledTimes(2);

      // Clicking Fullscreen and pressing F11 both toggle fullscreen mode + edge-to-edge frame
      await act(async () => {
        fireEvent.click(fullBtn);
      });
      expect(mockWindow.setFullscreen).toHaveBeenCalledWith(true);

      await act(async () => {
        fireEvent.keyDown(window, { key: 'F11' });
      });
      expect(mockWindow.setFullscreen).toHaveBeenCalledWith(false);

      await act(async () => {
        fireEvent.click(closeBtn);
      });
      expect(mockWindow.close).toHaveBeenCalledTimes(1);
    } finally {
      platformSpy.mockRestore();
    }

    cleanup();
    render(<WindowControls variant="windows" />);
    expect(document.querySelectorAll('.window-controls-win .win-control-btn').length).toBe(4);
  });
});
