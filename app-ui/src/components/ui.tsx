import React from 'react';

export const SectionTitle: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <div className="section-title">{children}</div>
);

export const Badge: React.FC<{ children: React.ReactNode; tone?: 'ok' | 'muted' | 'warn' }> = ({
  children,
  tone = 'muted',
}) => {
  const cls = tone === 'ok' ? 'badge badge-ok' : tone === 'warn' ? 'badge badge-warn' : 'badge';
  return <span className={cls}>{children}</span>;
};

export const ToggleRow: React.FC<{
  label: string;
  description?: string;
  checked: boolean;
  onChange: (next: boolean) => void;
}> = ({ label, description, checked, onChange }) => (
  <label className="toggle-row">
    <span>
      {label}
      {description && <div className="toggle-description">{description}</div>}
    </span>
    <input
      className="toggle-switch"
      type="checkbox"
      checked={checked}
      onChange={(e) => onChange(e.target.checked)}
    />
  </label>
);

export function SegmentedControl<T extends string | number>({
  value,
  options,
  onChange,
}: {
  value: T;
  options: ReadonlyArray<readonly [T, string]>;
  onChange: (next: T) => void;
}) {
  return (
    <div className="segmented-control">
      {options.map(([key, label]) => (
        <button
          key={String(key)}
          type="button"
          className="button"
          aria-pressed={value === key}
          onClick={() => onChange(key)}
        >
          {label}
        </button>
      ))}
    </div>
  );
}
