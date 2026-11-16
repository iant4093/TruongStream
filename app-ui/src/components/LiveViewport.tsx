import React, { useRef } from 'react';
import { Camera, Crosshair, Grid3X3, Play, SplitSquareHorizontal, Square } from 'lucide-react';
import { useStreamStore } from '../state/useStreamStore';
import { SegmentedControl } from './ui';

export const LiveViewport: React.FC = () => {
  const capabilities = useStreamStore((s) => s.capabilities);
  const telemetry = useStreamStore((s) => s.telemetry);
  const gradedFrameUrl = useStreamStore((s) => s.gradedFrameUrl);
  const ungradedFrameUrl = useStreamStore((s) => s.ungradedFrameUrl);
  const showThirdsGrid = useStreamStore((s) => s.showThirdsGrid);
  const splitEnabled = useStreamStore((s) => s.splitEnabled);
  const splitRatio = useStreamStore((s) => s.splitRatio);
  const focusReticle = useStreamStore((s) => s.focusReticle);
  const activeLutSummary = useStreamStore((s) => s.activeLutSummary);
  const lutConfig = useStreamStore((s) => s.lutConfig);
  const cleanOutputMode = useStreamStore((s) => s.cleanOutputMode);

  const toggleStream = useStreamStore((s) => s.toggleStream);
  const setShowThirdsGrid = useStreamStore((s) => s.setShowThirdsGrid);
  const setSplitEnabled = useStreamStore((s) => s.setSplitEnabled);
  const setSplitRatio = useStreamStore((s) => s.setSplitRatio);
  const setCameraProperty = useStreamStore((s) => s.setCameraProperty);
  const triggerAutofocus = useStreamStore((s) => s.triggerAutofocus);
  const resetToWholeAreaAf = useStreamStore((s) => s.resetToWholeAreaAf);
  const setCleanOutputMode = useStreamStore((s) => s.setCleanOutputMode);

  const stageRef = useRef<HTMLDivElement>(null);
  const draggingSplitRef = useRef(false);
  const suppressNextStageClickRef = useRef(false);

  const connected = telemetry?.connected ?? false;
  const streaming = telemetry?.streaming ?? false;
  const focusDesc = capabilities?.properties.focus_mode;
  const focusModeRaw = focusDesc?.current.raw ?? 0;
  const focusModeToggleValue = focusModeRaw === 1 ? 1 : 0;
  const frameW = telemetry?.width && telemetry.width > 0 ? telemetry.width : 16;
  const frameH = telemetry?.height && telemetry.height > 0 ? telemetry.height : 9;

  const updateSplitFromClientX = (clientX: number) => {
    if (!Number.isFinite(clientX) || !stageRef.current) return;
    const rect = stageRef.current.getBoundingClientRect();
    if (rect.width <= 0) return;
    setSplitRatio((clientX - rect.left) / rect.width);
  };

  const finishSplitDrag = () => {
    if (!draggingSplitRef.current) return;
    draggingSplitRef.current = false;
    window.setTimeout(() => {
      suppressNextStageClickRef.current = false;
    }, 0);
  };

  const onStageClick = (e: React.MouseEvent<HTMLDivElement>) => {
    if (cleanOutputMode) return;
    if (suppressNextStageClickRef.current) {
      suppressNextStageClickRef.current = false;
      return;
    }
    if (!connected || !stageRef.current) return;
    const rect = stageRef.current.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const nx = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const ny = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));

    // Invert aspect-fit crop, mirror/flip, and digital crop zoom so Touch AF targets the clicked pixel.
    const srcW = telemetry?.sourceWidth && telemetry.sourceWidth > 0 ? telemetry.sourceWidth : 3;
    const srcH = telemetry?.sourceHeight && telemetry.sourceHeight > 0 ? telemetry.sourceHeight : 2;
    const srcAspect = srcW / Math.max(srcH, 1);
    const dstAspect = frameW / Math.max(frameH, 1);
    const aspectScaleX = srcAspect > dstAspect ? dstAspect / srcAspect : 1.0;
    const aspectScaleY = dstAspect > srcAspect ? srcAspect / dstAspect : 1.0;
    const zoom = Math.max(lutConfig.crop, 1.0);
    let sx = lutConfig.mirror ? 1 - nx : nx;
    let sy = lutConfig.flip ? 1 - ny : ny;
    sx = Math.max(0.02, Math.min(0.98, (sx - 0.5) * (aspectScaleX / zoom) + 0.5));
    sy = Math.max(0.02, Math.min(0.98, (sy - 0.5) * (aspectScaleY / zoom) + 0.5));

    void triggerAutofocus(sx, sy, nx, ny);
  };

  const splitPct = Math.round(splitRatio * 100);

  return (
    <section className="stage-pane" aria-label="Live camera preview">
      <div className="stage-viewport">
        <div
          ref={stageRef}
          className="stage-frame-box"
          style={
            connected && !cleanOutputMode
              ? {
                  aspectRatio: `${frameW} / ${frameH}`,
                  width: `min(100%, calc((100vh - 160px) * ${frameW} / ${frameH}))`,
                }
              : undefined
          }
          role="button"
          tabIndex={0}
          aria-label="Live View canvas (click to autofocus)"
          onPointerMove={(e) => {
            if (draggingSplitRef.current) {
              updateSplitFromClientX(e.clientX);
            }
          }}
          onPointerUp={finishSplitDrag}
          onPointerCancel={finishSplitDrag}
          onClick={onStageClick}
          onDoubleClick={() => {
            if (cleanOutputMode) setCleanOutputMode(false);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              if (connected) void triggerAutofocus();
            }
          }}
        >
          {gradedFrameUrl ? (
            <>
              <img
                src={gradedFrameUrl}
                alt="Graded camera live view"
                className="stage-video-layer"
                draggable={false}
              />
              {splitEnabled && ungradedFrameUrl && (
                <>
                  <img
                    src={ungradedFrameUrl}
                    alt="Ungraded camera live view"
                    className="stage-video-layer"
                    style={{ clipPath: `inset(0 ${100 - splitPct}% 0 0)` }}
                    draggable={false}
                  />
                  <div
                    role="slider"
                    tabIndex={0}
                    aria-label="LUT split wipe position"
                    aria-valuemin={5}
                    aria-valuemax={95}
                    aria-valuenow={splitPct}
                    className="stage-split-handle"
                    style={{ left: `${splitPct}%` }}
                    onPointerDown={(e) => {
                      e.stopPropagation();
                      draggingSplitRef.current = true;
                      suppressNextStageClickRef.current = true;
                      e.currentTarget.setPointerCapture?.(e.pointerId);
                      updateSplitFromClientX(e.clientX);
                    }}
                    onPointerMove={(e) => {
                      if (!draggingSplitRef.current) return;
                      e.stopPropagation();
                      updateSplitFromClientX(e.clientX);
                    }}
                    onPointerUp={(e) => {
                      e.stopPropagation();
                      e.currentTarget.releasePointerCapture?.(e.pointerId);
                      finishSplitDrag();
                    }}
                    onPointerCancel={(e) => {
                      e.stopPropagation();
                      finishSplitDrag();
                    }}
                    onClick={(e) => e.stopPropagation()}
                    onKeyDown={(e) => {
                      if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') {
                        e.preventDefault();
                        e.stopPropagation();
                        setSplitRatio(splitRatio - 0.05);
                      } else if (e.key === 'ArrowRight' || e.key === 'ArrowUp') {
                        e.preventDefault();
                        e.stopPropagation();
                        setSplitRatio(splitRatio + 0.05);
                      }
                    }}
                  >
                    <span className="stage-split-grip" aria-hidden="true">
                      ‹ ›
                    </span>
                  </div>
                  <span className="stage-split-badge left">Original</span>
                  <span className="stage-split-badge right">{activeLutSummary?.title ?? 'LUT'}</span>
                </>
              )}
            </>
          ) : (
            <div className="stage-empty">
              <Camera size={28} strokeWidth={1.3} />
              <div>
                {connected
                  ? `${capabilities?.camera_model ?? 'Camera'} connected — start Live View to stream`
                  : 'Connect a Canon, Sony, Nikon, or Fujifilm camera over USB'}
              </div>
            </div>
          )}

          {!cleanOutputMode && connected && capabilities && !capabilities.manual_mode_active && (
            <div
              className="stage-mode-warning"
              role="status"
              onClick={(e) => e.stopPropagation()}
            >
              <strong>Manual Mode (M) Required</strong>
              <span>
                Camera dial is set to {capabilities.exposure_mode}. Turn your camera&apos;s mode
                dial to M for full exposure control.
              </span>
            </div>
          )}

          {!cleanOutputMode && showThirdsGrid && <div className="rule-of-thirds" aria-hidden="true" />}

          {!cleanOutputMode && focusReticle && (
            <button
              key={focusReticle.key}
              type="button"
              className="focus-reticle"
              aria-label="Unlock focus point and return to Whole Area AF"
              title="Click to unlock focus point and return to Whole Area AF"
              style={{
                left: `${focusReticle.x * 100}%`,
                top: `${focusReticle.y * 100}%`,
              }}
              onClick={(e) => {
                e.stopPropagation();
                void resetToWholeAreaAf();
              }}
            />
          )}
        </div>
      </div>

      {!cleanOutputMode && (
        <div className="stage-toolbar">
          <div className="stage-toolbar-group">
            <button
              type="button"
              className={`button ${streaming ? '' : 'button-primary'}`}
              disabled={!connected}
              onClick={() => void toggleStream()}
            >
              {streaming ? <Square size={12} /> : <Play size={12} />}
              <span>{streaming ? 'Stop Live View' : 'Start Live View'}</span>
            </button>

            <button
              type="button"
              className="button"
              disabled={!connected}
              onClick={() => void triggerAutofocus()}
            >
              <Crosshair size={13} />
              <span>AF Trigger</span>
            </button>

            {connected &&
              (focusReticle !== null || capabilities?.properties.focus_area?.current.raw === 1) && (
                <button
                  type="button"
                  className="button is-active"
                  onClick={() => void resetToWholeAreaAf()}
                >
                  <span>Whole Area AF</span>
                </button>
              )}

            {connected && (
              <SegmentedControl<number>
                value={focusModeToggleValue}
                options={[
                  [0, 'One-Shot'],
                  [1, 'AF-C'],
                ]}
                onChange={(rawValue) => {
                  if (focusDesc?.writable !== false) {
                    void setCameraProperty('focus_mode', rawValue);
                  }
                }}
              />
            )}
          </div>

          <div className="stage-toolbar-group">
            <button
              type="button"
              className={`button ${showThirdsGrid ? 'is-active' : ''}`}
              aria-pressed={showThirdsGrid}
              onClick={() => setShowThirdsGrid(!showThirdsGrid)}
            >
              <Grid3X3 size={13} />
              <span>Thirds Grid</span>
            </button>

            <button
              type="button"
              className={`button ${splitEnabled ? 'is-active' : ''}`}
              aria-pressed={splitEnabled}
              onClick={() => void setSplitEnabled(!splitEnabled)}
            >
              <SplitSquareHorizontal size={13} />
              <span>LUT Split</span>
            </button>
          </div>
        </div>
      )}
    </section>
  );
};
