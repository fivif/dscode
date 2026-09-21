# Third-party notices

DS Code is MIT licensed (see `LICENSE`). This file records the third-party work
that is incorporated into it — not merely depended upon — so the attribution
obligations of those licences are met when the app is distributed as a binary.

Dependencies pulled from crates.io and npm are **not** listed here; they are
enumerated by `Cargo.lock` and `package-lock.json` and carry their own notices in
the distributed artefacts.

---

## DeepSeek Harness (`deepseek-ai/deepseek-harness`)

**Licence:** MIT
**Copyright:** DeepSeek
**Source:** https://github.com/deepseek-ai/deepseek-harness

DeepSeek Harness (dsh) is a TypeScript pnpm monorepo. No dsh source file, module
or package is compiled into DS Code, and there is no runtime dependency on it.
What has been incorporated is design and, in the places noted, literal source
adapted by hand into this codebase's own idioms (Rust, and React + Tailwind
rather than Cordis + CSS Modules). Each site below was read in the upstream tree
before being adapted.

### Design tokens

The dark-theme half of the neutral scale in
`crates/dscode-desktop/ui/tailwind.config.js` is the dark half of dsh's
`--dsw-static-*` custom properties, remapped onto this project's semantic token
names. The four-tier text ramp (`primary` / `secondary` / `muted` / `faint`) and
the near-neutral, very slightly red-shifted greys are dsh's.

The elevation shape in the same file — a box-shadow whose **first layer is a
spread-only `0.5px` hairline stroke** rather than a `border`, so a raised surface
reads as one edge instead of a border stacked under a shadow — is likewise dsh's
`--dsw-elevation-stroke`.

### UI components

- `crates/dscode-desktop/ui/src/components/Chat/ThinkingBlock.tsx` — the
  collapsible reasoning row, adapted from
  `packages/client/ui-chat/src/client/chat/ReasoningRow.tsx`: collapsed by
  default; the summary line following the *latest* text while streaming and
  snapping back to the *first* once finished; a fixed 24px collapsed height; the
  absence of a card wrapper; and the fixed-length light band that sweeps the row
  while the model is still reasoning.
- `crates/dscode-desktop/ui/src/components/Chat/PermissionBanner.tsx`,
  `src/components/Sidebar/SessionItem.tsx` — surface treatment follows the same
  elevation rule.

### Icon glyphs

`ThinkingBlock.tsx` carries two filled 14px glyphs (`IconThink`,
`IconChevronDown`) taken from `packages/client/ui-primitives/src/icons/` — the
reasoning glyph verbatim, the chevron re-authored.

The multi-size filled-icon convention used by the set in
`crates/dscode-desktop/ui/src/components/icons/` — size suffix in the export
name, the viewBox following the size rather than a shared 24px grid, filled
`currentColor` paths — is dsh's `IconXxxNN` convention. The paths in that set are
this project's own drawings unless a site says otherwise.

### Markdown type ramp

`crates/dscode-desktop/ui/src/styles/globals.css` derives its markdown ladder
from dsh's `packages/client/ui-primitives/src/markdown/MarkdownText.module.css`.
CSS declarations are not themselves copyrightable and every value here has been
re-expressed in this project's tokens, but the derivation is recorded because the
*decisions* — 14/24 body, a 16px block gap, `0.875em` inline code on a `0.5px`
border, links that are weight-500 and undecorated at rest, table cells at
`10px 16px` with a `min(30vw, 320px)` cell cap — are dsh's.

### Not ported

dsh's signature `corner-shape: superellipse(1.5)` continuous-curvature radius was
evaluated and **rejected**: WebView2 does not implement `corner-shape`, so it
cannot be reproduced on this platform without a path-based fallback the rest of
the design does not justify.
