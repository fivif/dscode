import type { AppConfig, ProviderConfig } from '@/lib/types';
import { isProxyConfigured } from '@/lib/types';
import { IconChevronRight16 } from '@/components/icons';
import type {
  FieldContext,
  FieldDescriptor,
  FieldGroup,
  SectionId,
  SectionMeta,
} from './fieldSchema';
import { FieldList } from './fieldSchema';

/**
 * The field table: every setting the page owns, one entry each. Adding a field
 * means adding a descriptor here — no JSX edits at the call site.
 *
 * The descriptors are grouped by `group`; `GROUPS` below controls headings and
 * disclosures, so the reading order is also declared in one place.
 */

/** Channels that get a tab in the 渠道 section. Ollama has no tab today. */
export const PROVIDER_KEYS = ['deepseek', 'openai', 'anthropic'] as const;

export const PROVIDER_LABELS: Record<string, string> = {
  deepseek: 'DeepSeek',
  openai: 'OpenAI',
  anthropic: 'Anthropic',
  ollama: 'Ollama',
};

/**
 * Channels the image tool accepts — mirrors `CHANNELS` in
 * `dscode-core/src/tools/image.rs`, which is the authority: it refuses any key
 * outside its own list rather than falling back to another channel.
 *
 * Deliberately **not** `PROVIDER_KEYS` above: that list is the channel *tabs*
 * this page renders (no Ollama tab), which is narrower than what the tool takes.
 */
const IMAGE_PROVIDER_KEYS = ['deepseek', 'openai', 'anthropic', 'ollama'] as const;

/**
 * Fallback base URLs, mirroring `crates/dscode-core/src/config/providers.rs`
 * (`default_base_url`). Those constants exist so this placeholder and Rust's
 * `Default` impl agree — but they are not exposed over the `get_config`
 * contract, so they are repeated here. If you change one, change both.
 */
const DEFAULT_BASE_URL: Record<string, string> = {
  deepseek: 'https://api.deepseek.com/v1',
  openai: 'https://api.openai.com/v1',
  anthropic: 'https://api.anthropic.com',
  ollama: 'http://localhost:11434/v1',
};

/** Suggestions for the model field — free text stays allowed (see below). */
const IMAGE_MODEL_CANDIDATES = ['gpt-image-1', 'gpt-image-1-mini', 'dall-e-3', 'dall-e-2'];

/**
 * Legal `size` values, **per model**.
 *
 * These sets are not interchangeable. `512x512` is a `dall-e-2` size,
 * `1792x1024` a `dall-e-3` one, and `gpt-image-*` accepts neither — the
 * provider answers a foreign size with a 400, it does not downscale. A single
 * union list in the dropdown therefore makes an invalid pair one click away.
 */
const IMAGE_SIZES_BY_MODEL: { match: string; sizes: readonly string[] }[] = [
  { match: 'gpt-image', sizes: ['1024x1024', '1536x1024', '1024x1536', 'auto'] },
  { match: 'dall-e-3', sizes: ['1024x1024', '1792x1024', '1024x1792'] },
  { match: 'dall-e-2', sizes: ['256x256', '512x512', '1024x1024'] },
];

/**
 * Union of the sets above, used only for a model id we do not recognise.
 * A gateway can serve ids we have never heard of, so the user keeps the full
 * choice there and owns the consequence — narrowing it to a guess would block
 * a model that works.
 */
const IMAGE_SIZES_UNKNOWN = [
  '1024x1024',
  '1536x1024',
  '1024x1536',
  '1792x1024',
  '1024x1792',
  '512x512',
  '256x256',
];

/** Sizes the given model id accepts. Substring match, case-insensitive. */
function imageSizesForModel(model: string): readonly string[] {
  const m = (model || '').trim().toLowerCase();
  const hit = IMAGE_SIZES_BY_MODEL.find((e) => m.includes(e.match));
  return hit ? hit.sizes : IMAGE_SIZES_UNKNOWN;
}

/**
 * A `<select>` must never display a different value than the one actually saved.
 * A hand-edited config.toml can hold a value outside the list; keep it as an
 * (extra) option instead of silently showing the first one.
 */
function withCurrent(options: readonly string[], current: string): string[] {
  const cur = (current || '').trim();
  return cur && !options.includes(cur) ? [cur, ...options] : [...options];
}

const REASONING_TITLE =
  'DeepSeek/OpenAI: reasoning_effort；Claude: extended thinking budget_tokens';

const REASONING_OPTIONS = [
  { value: 'low', label: 'low', title: REASONING_TITLE },
  { value: 'medium', label: 'medium', title: REASONING_TITLE },
  { value: 'high', label: 'high', title: REASONING_TITLE },
  { value: 'max', label: 'max', title: REASONING_TITLE },
];

export const SECTIONS: SectionMeta[] = [
  { id: 'general', label: '通用', description: '默认模型与会话保留' },
  { id: 'generation', label: '生成', description: '推理深度、回复长度、温度与图像生成' },
  { id: 'context', label: '上下文', description: '上下文窗口与自动压缩的触发点' },
  { id: 'proxy', label: '网络代理', description: '模型、联网、MCP 与 Skill 下载的代理出口' },
  { id: 'channels', label: '渠道', description: '各渠道的密钥、接口地址与上架模型' },
];

export const GROUPS: FieldGroup[] = [
  { id: 'general', section: 'general' },
  { id: 'generation', section: 'generation' },
  {
    id: 'image',
    section: 'generation',
    label: '图像生成',
    separated: true,
    note: (
      <p className="text-[11px] text-warning leading-snug">
        图像生成按张计费，每次调用都会向渠道产生费用（与对话 token 分开结算）。
        生成的图片保存在 ~/.dscode/images/，并直接显示在对话里。
      </p>
    ),
  },
  {
    id: 'image-advanced',
    section: 'generation',
    collapsible: true,
    summary: (
      <>
        高级设置
        <span className="text-faint"> · 一般不用改</span>
      </>
    ),
    intro: (
      <>
        <p className="text-[11px] text-muted leading-snug">
          这三项决定「出图请求怎么发出去」，大多保持默认即可：
        </p>
        <ul className="space-y-1 text-[11px] text-muted leading-snug">
          <li>
            · <span className="text-secondary">图像模型</span> —— 只在「用文本模型聊天、由模型自己
            决定调用生图工具」时作为默认模型。直接在对话的模型选择器里选中生图模型时，
            用的是你选的那个模型，这一项不参与。
          </li>
          <li>
            · <span className="text-secondary">图像尺寸</span>、
            <span className="text-secondary">使用渠道</span> —— 两种方式都会读这里的值
            （直接选生图模型时也一样）。尺寸默认 1024x1024，对这里所有模型都合法。
          </li>
        </ul>
      </>
    ),
  },
  { id: 'context', section: 'context' },
  { id: 'proxy', section: 'proxy' },
];

/** Channel-scoped helpers — `providerKey` is set by the 渠道 section only. */
function providerOf(ctx: FieldContext): ProviderConfig | undefined {
  return ctx.config.providers[ctx.providerKey || 'deepseek'];
}

function setProviderOf(ctx: FieldContext, patch: Partial<ProviderConfig>) {
  ctx.setProvider(ctx.providerKey || 'deepseek', patch);
}

export const FIELDS: FieldDescriptor[] = [
  // ── 通用 ──────────────────────────────────────────────────────────────────
  {
    id: 'default_model',
    section: 'general',
    label: '默认模型',
    control: 'select',
    options: (ctx) =>
      ctx.defaultModelOptions.length === 0
        ? [{ value: '', label: '请先启用渠道并「获取列表」' }]
        : ctx.defaultModelOptions.map((m) => ({
            value: m.id,
            label: `${m.label} (${m.provider})`,
          })),
    disabledWhen: (ctx) => ctx.defaultModelOptions.length === 0,
    // Same source as the input box and the channel model picker.
    get: (ctx) =>
      ctx.defaultModelOptions.some((m) => m.id === ctx.config.default_model)
        ? ctx.config.default_model
        : ctx.defaultModelOptions[0]?.id || '',
    set: (ctx, id: string) => {
      const opt = ctx.defaultModelOptions.find((m) => m.id === id);
      ctx.setDefaultModel(id, opt?.provider);
    },
    help: (ctx) => (
      <>
        仅已启用渠道 · 已勾选上架的模型 · 与输入框同一数据源
        {ctx.config.active_provider
          ? ` · 当前渠道：${
              PROVIDER_LABELS[ctx.config.active_provider] || ctx.config.active_provider
            }`
          : ''}
        {ctx.defaultModelOptions.length
          ? ` · 共 ${ctx.defaultModelOptions.length} 个可选`
          : ' · 请在渠道页获取列表并勾选上架'}
      </>
    ),
  },
  {
    id: 'retention_days',
    section: 'general',
    label: '会话保留天数',
    control: 'number',
    min: 1,
    max: 365,
    widthClass: 'w-24',
    deferred: true,
    // An empty or non-numeric draft reads as "leave it at the default" — the
    // same `parseInt(...) || 30` the hand-written field used.
    coerce: (raw) => parseInt(raw, 10) || 30,
    get: (ctx) => ctx.config.retention_days,
    set: (ctx, v: number) => ctx.set({ retention_days: v }),
  },

  // ── 生成 ──────────────────────────────────────────────────────────────────
  {
    id: 'reasoning_effort',
    section: 'generation',
    label: '推理深度',
    control: 'segmented',
    options: REASONING_OPTIONS,
    get: (ctx) => ctx.config.reasoning_effort,
    set: (ctx, v: string) => ctx.set({ reasoning_effort: v }),
    help: (
      <span className="block max-w-md">
        DeepSeek/OpenAI 兼容：发 reasoning_effort（max→high）。Claude 原生：extended thinking
        budget（low≈4k … max≈32k tokens）。调高更慢更贵，可能出现「思考过程」块。
      </span>
    ),
  },
  {
    id: 'max_tokens_enabled',
    section: 'generation',
    label: '回复长度限制',
    control: 'toggle',
    caption: (on) => (on ? '已限制' : '不限制（默认）'),
    get: (ctx) => ctx.config.max_tokens > 0,
    set: (ctx, on: boolean) => ctx.set({ max_tokens: on ? 8192 : 0 }),
  },
  {
    id: 'max_tokens',
    section: 'generation',
    label: '上限 tokens',
    control: 'number',
    min: 256,
    max: 128000,
    widthClass: 'w-28',
    deferred: true,
    visibleWhen: (ctx) => ctx.config.max_tokens > 0,
    // Clearing the field means "unlimited" on the Rust side — same as turning
    // the toggle above off, so the row disappears with it. The `8192` for
    // unparseable text and the `0` for empty are the hand-written field's rules.
    coerce: (raw) => {
      if (raw.trim() === '') return 0;
      const n = parseInt(raw, 10);
      return Number.isNaN(n) ? 8192 : n;
    },
    get: (ctx) => ctx.config.max_tokens,
    set: (ctx, v: number) => ctx.set({ max_tokens: v }),
  },
  {
    id: 'temperature',
    section: 'generation',
    label: '温度',
    control: 'range',
    min: 0,
    max: 2,
    step: 0.1,
    widthClass: 'w-36',
    minLabel: '0.0',
    maxLabel: '2.0',
    format: (v) => v.toFixed(1),
    deferred: true,
    get: (ctx) => ctx.config.temperature,
    set: (ctx, v: number) => ctx.set({ temperature: v }),
  },

  // ── 生成 · 图像 ───────────────────────────────────────────────────────────
  {
    id: 'image_enabled',
    section: 'generation',
    group: 'image',
    label: '启用图像生成',
    control: 'toggle',
    caption: (on) => (on ? '已启用' : '已关闭'),
    get: (ctx) => ctx.config.image_enabled,
    set: (ctx, on: boolean) => ctx.set({ image_enabled: on }),
    help: (
      <span className="block leading-snug">
        关闭后工具不会被注册给模型 —— 模型看不到、也无法调用它，它的定义 token 也不再随每次请求发送。
        这是总开关：直接在对话里选中生图模型时走的是同一个工具，同样会被拒绝。
      </span>
    ),
  },
  {
    id: 'image_model',
    section: 'generation',
    group: 'image-advanced',
    label: '图像模型',
    control: 'text',
    mono: true,
    placeholder: 'gpt-image-1',
    candidates: IMAGE_MODEL_CANDIDATES,
    deferred: true,
    disabledWhen: (ctx) => !ctx.config.image_enabled,
    get: (ctx) => ctx.config.image_model,
    set: (ctx, model: string) => {
      // Changing the model can strand `image_size` on a value the new one
      // rejects (`512x512` is dall-e-2 only, `1792x1024` dall-e-3 only) — the
      // provider answers that with a 400, it does not downscale. The correction
      // rides in the *same* patch as the model so the two can never be
      // persisted out of step; a separate size write would carry the old model.
      const sizes = imageSizesForModel(model);
      const patch: Partial<AppConfig> = { image_model: model };
      if (ctx.config.image_size && !sizes.includes(ctx.config.image_size)) {
        patch.image_size = sizes[0];
      }
      ctx.set(patch);
    },
    help: (
      <span className="block leading-snug">
        填中转站「模型列表」里实际列出的 id —— 写错只会在出图时报错，不会回退到别的模型。
        候选只是建议，可以随便填。尺寸选项会跟着上面这个 id 变。
      </span>
    ),
  },
  {
    id: 'image_size',
    section: 'generation',
    group: 'image-advanced',
    label: '图像尺寸',
    control: 'select',
    options: (ctx) =>
      withCurrent(imageSizesForModel(ctx.config.image_model), ctx.config.image_size).map((s) => ({
        value: s,
        label: s === 'auto' ? 'auto（由模型决定）' : s,
      })),
    disabledWhen: (ctx) => !ctx.config.image_enabled,
    get: (ctx) => ctx.config.image_size,
    set: (ctx, v: string) => ctx.set({ image_size: v }),
    help: (
      <span className="block leading-snug">
        可选尺寸由模型决定，各模型不通用 —— 选错会被渠道以 400 拒绝，不会自动缩放。
      </span>
    ),
    note: (ctx) =>
      ctx.config.image_size &&
      !imageSizesForModel(ctx.config.image_model).includes(ctx.config.image_size) ? (
        <p className="text-[11px] text-warning leading-snug">
          当前尺寸不是「{ctx.config.image_model || '该模型'}」接受的取值（多半来自旧配置或手改的
          config.toml），直接出图会被拒绝 —— 请从上面的列表里选一个。
        </p>
      ) : null,
  },
  {
    id: 'image_provider',
    section: 'generation',
    group: 'image-advanced',
    label: '使用渠道',
    control: 'select',
    options: (ctx) => [
      { value: '', label: '跟随当前渠道（默认）' },
      ...withCurrent(IMAGE_PROVIDER_KEYS, ctx.config.image_provider).map((k) => ({
        value: k,
        label: PROVIDER_LABELS[k] || k,
      })),
    ],
    disabledWhen: (ctx) => !ctx.config.image_enabled,
    get: (ctx) => ctx.config.image_provider,
    set: (ctx, v: string) => ctx.set({ image_provider: v }),
    help: (
      <span className="block leading-snug">
        出图用的是该渠道自己的 base_url 与 API Key；留空就跟随默认模型所在渠道。
        指定了渠道但它没有 Key 时会直接报错，不会拿别的渠道的 Key 顶替。
      </span>
    ),
    note: (ctx) =>
      ctx.config.image_provider &&
      !(ctx.config.providers[ctx.config.image_provider]?.api_key || '').trim() ? (
        <p className="text-[11px] text-warning leading-snug">
          该渠道还没填 API Key —— 现在出图会直接失败。
        </p>
      ) : null,
  },

  // ── 上下文 ────────────────────────────────────────────────────────────────
  {
    id: 'context_window_tokens',
    section: 'context',
    label: '上下文窗口',
    control: 'select',
    options: [
      { value: 128000, label: '128K' },
      { value: 256000, label: '256K' },
      { value: 512000, label: '512K' },
      { value: 1000000, label: '1M (默认)' },
    ],
    get: (ctx) => ctx.config.context_window_tokens,
    set: (ctx, v: string) => ctx.set({ context_window_tokens: Number(v) }),
    help: (
      <span className="block leading-relaxed">
        填你模型<strong>真实的</strong>窗口大小 —— 压缩的触发点是按它算的。
        <strong>填大了比填小更糟</strong>：闸门永远到不了，provider 先拒绝，整个回合失败；
        填小了只是压缩早一点触发。不确定就从 128K 起，看「已自动压缩」提示出现的时机再往上调。
      </span>
    ),
  },
  {
    id: 'context_compress_threshold',
    section: 'context',
    label: '压缩阈值',
    control: 'range',
    min: 0.6,
    max: 0.85,
    step: 0.05,
    widthClass: 'w-36',
    minLabel: '60%',
    maxLabel: '85%',
    format: (v) => `${(v * 100).toFixed(0)}%`,
    deferred: true,
    get: (ctx) => ctx.config.context_compress_threshold,
    set: (ctx, v: number) => ctx.set({ context_compress_threshold: v }),
    help: (
      <span className="block leading-relaxed">
        上限 85%：最深的一级要在这个值之上再留 15% 的余量。
        另外，<strong>不可逆</strong>的旧工具输出剪裁会比这个值早 20% 开始。
      </span>
    ),
  },

  // ── 网络代理 ──────────────────────────────────────────────────────────────
  {
    id: 'proxy_url',
    section: 'proxy',
    label: '代理地址',
    control: 'text',
    mono: true,
    placeholder: 'http://127.0.0.1:7890 或 socks5://127.0.0.1:1080',
    deferred: true,
    get: (ctx) => ctx.config.proxy?.url || '',
    set: (ctx, v: string) => ctx.set({ proxy: { ...ctx.config.proxy, url: v } }),
    help: (ctx) => (
      <>
        支持 http / https / socks5。留空表示未配置；未配置时各处代理开关不可用。
        {isProxyConfigured(ctx.config.proxy?.url) ? (
          <span className="text-success"> · 已识别有效代理</span>
        ) : ctx.config.proxy?.url?.trim() ? (
          <span className="text-warning"> · 地址无效</span>
        ) : null}
      </>
    ),
  },
  {
    id: 'proxy_global',
    section: 'proxy',
    label: '全局启用',
    control: 'toggle',
    disabledWhen: (ctx) => !isProxyConfigured(ctx.config.proxy?.url),
    get: (ctx) => !!ctx.config.proxy?.global && isProxyConfigured(ctx.config.proxy?.url),
    set: (ctx, on: boolean) => ctx.set({ proxy: { ...ctx.config.proxy, global: on } }),
    caption: () => '强制全软件走代理（模型 / 联网 / MCP / Skill 全部启用，各处不可单独关闭）',
  },
  {
    id: 'proxy_web',
    section: 'proxy',
    label: '联网工具',
    control: 'toggle',
    disabledWhen: (ctx) =>
      !isProxyConfigured(ctx.config.proxy?.url) || !!ctx.config.proxy?.global,
    get: (ctx) =>
      isProxyConfigured(ctx.config.proxy?.url) &&
      (!!ctx.config.proxy?.global || !!ctx.config.proxy?.web_use_proxy),
    set: (ctx, on: boolean) =>
      ctx.set({ proxy: { ...ctx.config.proxy, web_use_proxy: on } }),
    caption: (_on, ctx) => (
      <>
        联网默认走代理（Agent 仍可在每次调用时用 use_proxy 自行覆盖）
        {ctx.config.proxy?.global ? ' · 全局默认开' : ''}
      </>
    ),
  },
  {
    id: 'mcp_use_proxy',
    section: 'proxy',
    label: 'MCP 连接',
    control: 'toggle',
    disabledWhen: (ctx) =>
      !isProxyConfigured(ctx.config.proxy?.url) || !!ctx.config.proxy?.global,
    get: (ctx) =>
      isProxyConfigured(ctx.config.proxy?.url) &&
      (!!ctx.config.proxy?.global || !!ctx.config.mcp_use_proxy),
    set: (ctx, on: boolean) => ctx.set({ mcp_use_proxy: on }),
    caption: (_on, ctx) => (
      <>
        MCP 进程（npx 等）使用代理
        {ctx.config.proxy?.global ? ' · 全局已强制' : ''}
      </>
    ),
  },
  {
    id: 'skills_use_proxy',
    section: 'proxy',
    label: 'Skill 下载',
    control: 'toggle',
    disabledWhen: (ctx) =>
      !isProxyConfigured(ctx.config.proxy?.url) || !!ctx.config.proxy?.global,
    get: (ctx) =>
      isProxyConfigured(ctx.config.proxy?.url) &&
      (!!ctx.config.proxy?.global || !!ctx.config.skills_use_proxy),
    set: (ctx, on: boolean) => ctx.set({ skills_use_proxy: on }),
    caption: (_on, ctx) => (
      <>
        第三方 Skill git 克隆走代理
        {ctx.config.proxy?.global ? ' · 全局已强制' : ''}
      </>
    ),
  },
];

/**
 * Per-channel fields, rendered inside the 渠道 tabs with `ctx.providerKey` set.
 * The scanned-model checklist below them is bespoke and lives in
 * `ChannelSection.tsx`.
 */
export const CHANNEL_FIELDS: FieldDescriptor[] = [
  {
    id: 'provider_enabled',
    section: 'channels',
    label: '启用',
    control: 'toggle',
    caption: (_on, ctx) => `启用 ${PROVIDER_LABELS[ctx.providerKey || ''] || ctx.providerKey}`,
    get: (ctx) => !!providerOf(ctx)?.enabled,
    set: (ctx, on: boolean) => setProviderOf(ctx, { enabled: on }),
  },
  {
    id: 'provider_use_proxy',
    section: 'channels',
    label: '使用代理',
    control: 'toggle',
    disabledWhen: (ctx) =>
      !isProxyConfigured(ctx.config.proxy?.url) || !!ctx.config.proxy?.global,
    get: (ctx) =>
      isProxyConfigured(ctx.config.proxy?.url) &&
      (!!ctx.config.proxy?.global || !!providerOf(ctx)?.use_proxy),
    set: (ctx, on: boolean) => setProviderOf(ctx, { use_proxy: on }),
    caption: (_on, ctx) => (
      <>
        该渠道请求走代理
        {!isProxyConfigured(ctx.config.proxy?.url)
          ? '（请先配置有效代理）'
          : ctx.config.proxy?.global
            ? ' · 全局已强制'
            : ''}
      </>
    ),
  },
  {
    id: 'provider_api_key',
    section: 'channels',
    label: 'API 密钥',
    control: 'password',
    mono: true,
    placeholder: 'sk-...',
    deferred: true,
    get: (ctx) => providerOf(ctx)?.api_key || '',
    set: (ctx, v: string) => setProviderOf(ctx, { api_key: v }),
  },
  {
    id: 'provider_base_url',
    section: 'channels',
    label: '接口地址',
    control: 'text',
    mono: true,
    deferred: true,
    placeholder: 'https://api.example.com/v1',
    get: (ctx) => providerOf(ctx)?.base_url || '',
    set: (ctx, v: string) => setProviderOf(ctx, { base_url: v }),
    help: (ctx) => (
      <>
        默认
        <span className="font-mono"> {DEFAULT_BASE_URL[ctx.providerKey || ''] || '—'}</span>
      </>
    ),
  },
];

/**
 * A section's groups, in declaration order, with empty groups dropped and
 * `visibleWhen` applied once (the group needs to know if it still has rows).
 */
export function SectionFields({ section, ctx }: { section: SectionId; ctx: FieldContext }) {
  const groups = GROUPS.filter((g) => g.section === section);
  return (
    <div className="space-y-5">
      {groups.map((g) => {
        const fields = FIELDS.filter(
          (f) =>
            f.section === section &&
            (f.group ?? f.section) === g.id &&
            (!f.visibleWhen || f.visibleWhen(ctx)),
        );
        if (fields.length === 0 && !g.label && !g.intro && !g.note) return null;

        const head = g.label ? (
          <div className={g.separated ? 'pt-5 border-t border-divider' : ''}>
            <h4 className="text-[13px] font-semibold text-primary">{g.label}</h4>
            {g.help ? <p className="text-[11px] text-muted mt-1">{g.help}</p> : null}
          </div>
        ) : null;

        const body = (
          <>
            {head}
            {g.intro ? (
              <div className="space-y-2">{g.intro}</div>
            ) : null}
            {fields.length > 0 ? <FieldList fields={fields} ctx={ctx} /> : null}
            {g.note}
          </>
        );

        if (g.collapsible) {
          return (
            <details key={g.id} className="group">
              <summary className="flex items-center gap-1.5 cursor-pointer select-none list-none [&::-webkit-details-marker]:hidden text-[11px] text-muted hover:text-secondary transition-colors rounded-control focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50">
                <IconChevronRight16
                  size={12}
                  className="shrink-0 transition-transform group-open:rotate-90"
                />
                <span>{g.summary}</span>
              </summary>
              <div className="mt-3 space-y-5">{body}</div>
            </details>
          );
        }

        return (
          <div key={g.id} className="space-y-5">
            {body}
          </div>
        );
      })}
    </div>
  );
}
