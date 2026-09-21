import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import type { AppConfig, ProviderConfig } from '@/lib/types';
import type { ModelOption } from '@/lib/models';

/**
 * Field descriptor schema + the one renderer that turns a descriptor into a
 * control. The descriptor *table* lives in `settingsFields.tsx`; this file is
 * where a control type is implemented, so adding a `control:` kind and the
 * fields that use it is still two edits, but adding a *field* is one entry.
 */

/**
 * Commit window for free-text / number / range fields. Matches the store's own
 * coalescing window (`configStore.saveConfig`), so a field commit and the disk
 * write land on the same tick instead of stacking two debounces.
 */
export const FIELD_COMMIT_DELAY_MS = 300;

export type SectionId = 'general' | 'generation' | 'context' | 'proxy' | 'channels';

export type ControlKind =
  | 'toggle'
  | 'text'
  | 'password'
  | 'number'
  | 'select'
  | 'range'
  /** A row of mutually exclusive buttons (reasoning depth). */
  | 'segmented';

export type FieldValue = string | number | boolean;

export interface FieldOption {
  value: string | number;
  label: string;
  /** Native tooltip, for options whose meaning needs a sentence. */
  title?: string;
}

/**
 * Everything a descriptor may read or write. `providerKey` is set only while a
 * channel-scoped field (see `CHANNEL_FIELDS`) is rendered.
 */
export interface FieldContext {
  config: AppConfig;
  fetchedModels: Record<string, string[]>;
  defaultModelOptions: readonly ModelOption[];
  providerKey?: string;
  set: (patch: Partial<AppConfig>) => void;
  setProvider: (key: string, patch: Partial<ProviderConfig>) => void;
  setDefaultModel: (id: string, provider?: string) => void;
}

export type FieldHelp = ReactNode | ((ctx: FieldContext) => ReactNode);

export interface FieldDescriptor {
  id: string;
  section: SectionId;
  /** Group id inside the section; defaults to the section id. See `GROUPS`. */
  group?: string;
  label: string;
  help?: FieldHelp;
  control: ControlKind;
  /** Commit through the per-field 300 ms debounce instead of on every change. */
  deferred?: boolean;
  placeholder?: string;
  mono?: boolean;
  options?: readonly FieldOption[] | ((ctx: FieldContext) => readonly FieldOption[]);
  /** number / range */
  min?: number;
  max?: number;
  step?: number;
  minLabel?: string;
  maxLabel?: string;
  /**
   * number only: raw input text → stored value. `null` leaves the stored value
   * untouched (an empty/invalid draft is dropped, and blur restores it).
   */
  coerce?: (raw: string) => number | null;
  /** range only: how the live value is displayed next to the slider. */
  format?: (value: number) => string;
  /** Text fields: suggestions rendered as a `<datalist>`. */
  candidates?: readonly string[];
  widthClass?: string;
  disabledWhen?: (ctx: FieldContext) => boolean;
  visibleWhen?: (ctx: FieldContext) => boolean;
  /** Toggle only: the wording next to the checkbox. */
  caption?: (value: boolean, ctx: FieldContext) => ReactNode;
  /** Extra node below the control (warnings, counters). */
  note?: (ctx: FieldContext) => ReactNode;
  get: (ctx: FieldContext) => FieldValue;
  // `any` because the stored value's type depends on `control`.
  set: (ctx: FieldContext, value: any) => void;
}

/**
 * A heading / disclosure / trailer paragraph inside a section. Fields join a
 * group by id; groups render in the order they are declared.
 */
export interface FieldGroup {
  id: string;
  section: SectionId;
  /** Heading above the group's fields. */
  label?: string;
  /** One-line description under `label`. */
  help?: string;
  /** Static block rendered before the fields (disclosure explainers). */
  intro?: ReactNode;
  /** Renders the group as a collapsed-by-default `<details>`. */
  collapsible?: boolean;
  /** The `<summary>` line when `collapsible`. */
  summary?: ReactNode;
  /** Static block rendered after the fields (billing / consequence warnings). */
  note?: ReactNode;
  /** Draw the top hairline (used when a group follows another in one panel). */
  separated?: boolean;
}

export interface SectionMeta {
  id: SectionId;
  label: string;
  description: string;
}

/**
 * Mirror of one value while its debounced commit is in flight.
 *
 * A field that commits on a delay cannot be driven straight from the store:
 * the store's value is 300 ms behind the keystroke, so a controlled input would
 * snap back to the old value on every render. Two rules make the mirror safe:
 *
 *   · while a commit is pending, an external change does **not** overwrite the
 *     draft (the echo of our own write must not fight the newer keystroke);
 *   · a pending commit is flushed on unmount, so switching section — or leaving
 *     settings — persists the edit instead of dropping it with the row.
 *
 * Timers are per field: two debounced fields edited in quick succession no
 * longer cancel each other (the old page shared one `debounceRef` for all of
 * them, so the first edit was silently lost).
 */
function useFieldValue<T>(
  external: T,
  commit: (value: T) => void,
  deferred: boolean,
  delay: number = FIELD_COMMIT_DELAY_MS,
): readonly [T, (value: T) => void, () => void] {
  const [draft, setDraft] = useState<T>(external);
  const draftRef = useRef<T>(external);
  const externalRef = useRef<T>(external);
  const commitRef = useRef(commit);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingRef = useRef(false);

  externalRef.current = external;
  commitRef.current = commit;

  const push = useCallback((value: T) => {
    draftRef.current = value;
    setDraft(value);
  }, []);

  useEffect(() => {
    if (!pendingRef.current) push(external);
  }, [external, push]);

  const flush = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    if (!pendingRef.current) return;
    pendingRef.current = false;
    commitRef.current(draftRef.current);
  }, []);

  const update = useCallback(
    (value: T) => {
      push(value);
      if (!deferred) {
        pendingRef.current = false;
        commitRef.current(value);
        return;
      }
      pendingRef.current = true;
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(flush, delay);
    },
    [deferred, delay, flush, push],
  );

  /** Drop the draft and an in-flight commit; used on blur of an invalid number. */
  const reset = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    pendingRef.current = false;
    push(externalRef.current);
  }, [push]);

  useEffect(() => () => flush(), [flush]);

  return [draft, update, reset] as const;
}

/** The label/control row. Also used directly for the channel scan widget. */
export function FieldRow({
  label,
  action,
  help,
  note,
  children,
}: {
  label: string;
  action?: ReactNode;
  help?: ReactNode;
  note?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex items-start gap-6">
      <div className="w-28 shrink-0 pt-2.5 flex items-center justify-between gap-2">
        <span className="text-[13px] text-primary">{label}</span>
        {action}
      </div>
      <div className="flex-1 min-w-0">
        {children}
        {help ? <div className="mt-1.5 text-[11px] text-muted leading-relaxed">{help}</div> : null}
        {note ? <div className="mt-1.5">{note}</div> : null}
      </div>
    </div>
  );
}

function optionsFor(
  descriptor: FieldDescriptor,
  ctx: FieldContext,
): readonly FieldOption[] {
  const { options } = descriptor;
  if (!options) return [];
  return typeof options === 'function' ? options(ctx) : options;
}

function numberFrom(descriptor: FieldDescriptor, raw: string): number | null {
  if (descriptor.coerce) return descriptor.coerce(raw);
  // Radix is explicit: `parseInt('08')` is 8 either way, but `parseInt('0x10')`
  // is 16 without one and 0 with it — the config stores decimal tokens only.
  const n = parseInt(raw, 10);
  return Number.isNaN(n) ? null : n;
}

/** One descriptor → one row. */
function SettingsRow({
  descriptor,
  ctx,
}: {
  descriptor: FieldDescriptor;
  ctx: FieldContext;
}) {
  const descriptorRef = useRef(descriptor);
  const ctxRef = useRef(ctx);
  descriptorRef.current = descriptor;
  ctxRef.current = ctx;

  const external = descriptor.get(ctx);
  const commit = useCallback((value: FieldValue) => {
    descriptorRef.current.set(ctxRef.current, value);
  }, []);
  const [draft, update, reset] = useFieldValue<FieldValue>(
    external,
    commit,
    !!descriptor.deferred,
  );
  // Deferred controls read the mirrored draft (the store is 300 ms behind the
  // keystroke); immediate ones stay store-driven, so a change the store refuses
  // (e.g. a blocked model pick) snaps back instead of showing a value that was
  // never accepted.
  const value = descriptor.deferred ? draft : external;

  const disabled = descriptor.disabledWhen?.(ctx) ?? false;
  const opts = optionsFor(descriptor, ctx);
  const help = typeof descriptor.help === 'function' ? descriptor.help(ctx) : descriptor.help;
  const note = descriptor.note ? descriptor.note(ctx) : null;
  const listId = `dscode-field-${descriptor.id}`;

  let control: ReactNode;
  switch (descriptor.control) {
    case 'toggle':
      control = (
        <label
          className={`flex items-center gap-2 ${
            disabled ? 'opacity-40 cursor-not-allowed' : 'cursor-pointer'
          }`}
        >
          <input
            type="checkbox"
            className="w-4 h-4 rounded accent-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
            checked={!!value}
            disabled={disabled}
            onChange={(e) => update(e.target.checked)}
          />
          {descriptor.caption ? (
            <span className="text-[13px] text-secondary">{descriptor.caption(!!value, ctx)}</span>
          ) : null}
        </label>
      );
      break;

    case 'segmented':
      control = (
        <div className="flex flex-wrap gap-1.5">
          {opts.map((o) => {
            const selected = String(value) === String(o.value);
            return (
              <button
                key={String(o.value)}
                type="button"
                title={o.title}
                disabled={disabled}
                className={`px-4 py-2 rounded-control text-[13px] font-mono transition-colors disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                  selected
                    ? 'bg-accent-soft text-primary ring-1 ring-accent/40'
                    : 'bg-card text-secondary hover:text-primary hover:bg-hover border border-border'
                }`}
                onClick={() => update(String(o.value))}
              >
                {o.label}
              </button>
            );
          })}
        </div>
      );
      break;

    case 'select':
      control = (
        <select
          className={`field disabled:opacity-40 ${descriptor.widthClass ?? ''}`}
          value={String(value)}
          disabled={disabled}
          onChange={(e) => update(e.target.value)}
        >
          {opts.map((o) => (
            <option key={String(o.value)} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      );
      break;

    case 'range':
      control = (
        <div>
          <div className="flex items-center gap-3">
            <input
              type="range"
              className={`accent-accent rounded-full disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                descriptor.widthClass ?? 'w-36'
              }`}
              min={descriptor.min}
              max={descriptor.max}
              step={descriptor.step}
              value={Number(value)}
              disabled={disabled}
              onChange={(e) => update(parseFloat(e.target.value))}
            />
            {descriptor.format ? (
              <span className="text-[13px] text-primary font-mono tabular-nums w-10">
                {descriptor.format(Number(value))}
              </span>
            ) : null}
          </div>
          {descriptor.minLabel || descriptor.maxLabel ? (
            <div
              className={`flex justify-between mt-1 text-[11px] text-muted ${
                descriptor.widthClass ?? 'w-36'
              }`}
            >
              <span>{descriptor.minLabel}</span>
              <span>{descriptor.maxLabel}</span>
            </div>
          ) : null}
        </div>
      );
      break;

    case 'number':
      control = (
        <input
          type="number"
          className={`field disabled:opacity-40 ${descriptor.widthClass ?? 'w-28'}`}
          min={descriptor.min}
          max={descriptor.max}
          step={descriptor.step}
          value={String(value)}
          disabled={disabled}
          onChange={(e) => update(e.target.value)}
          onBlur={() => {
            if (numberFrom(descriptor, String(value)) === null) reset();
          }}
        />
      );
      break;

    case 'password':
    case 'text':
    default:
      control = (
        <>
          <input
            type={descriptor.control === 'password' ? 'password' : 'text'}
            className={`field disabled:opacity-40 ${descriptor.mono ? 'font-mono' : ''} ${
              descriptor.widthClass ?? ''
            }`}
            placeholder={descriptor.placeholder}
            value={String(value)}
            disabled={disabled}
            list={descriptor.candidates ? listId : undefined}
            onChange={(e) => update(e.target.value)}
          />
          {descriptor.candidates ? (
            <datalist id={listId}>
              {descriptor.candidates.map((c) => (
                <option key={c} value={c} />
              ))}
            </datalist>
          ) : null}
        </>
      );
      break;
  }

  return (
    <FieldRow label={descriptor.label} help={help} note={note}>
      {control}
    </FieldRow>
  );
}

/**
 * Renders an already-filtered list. `visibleWhen` is applied by the caller
 * (it also has to decide whether a group is empty), so nothing here is
 * conditional on hooks.
 */
export function FieldList({
  fields,
  ctx,
}: {
  fields: readonly FieldDescriptor[];
  ctx: FieldContext;
}) {
  return (
    <div className="space-y-5">
      {fields.map((f) => (
        <SettingsRow key={f.id} descriptor={f} ctx={ctx} />
      ))}
    </div>
  );
}
