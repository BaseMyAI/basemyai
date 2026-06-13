# BaseMyAI — Branding Kit

**Blacksite Memory** · Private AI infrastructure · 100 % local · Built in Rust

---

## Contents

- [Brand Identity](#brand-identity)
- [Color Palette](#color-palette)
- [Logo](#logo)
- [Icons](#icons)
- [Social Icons](#social-icons)
- [Feature Images](#feature-images)
- [Tailwind CSS Integration](#tailwind-css-integration)

---

## Brand Identity

BaseMyAI is a **privacy-first, local-first** memory engine for AI agents. The visual language reflects that posture: near-black backgrounds ("Blacksite"), a single sharp accent (lime **Memory** green), and restrained secondary colors that signal trust and encryption without decoration.

**Do not** introduce gradients, shadows, or accent colors outside the defined palette. The brand communicates through restraint.

---

## Color Palette

### Primitives

| Name | Swatch | Hex | Usage |
|------|--------|-----|-------|
| **memory-400** | `#d7ff3f` | `#d7ff3f` | Primary CTA, active highlights |
| **memory-500** | `#bde800` | `#bde800` | Primary brand signal, icons, badge fills |
| **memory-600** | `#91b800` | `#91b800` | Hover / pressed state on light bg |
| **secure-400** | `#1bc3a3` | `#1bc3a3` | Live status, encryption indicator |
| **secure-500** | `#00a88a` | `#00a88a` | Success, privacy, isolation |
| **secure-600** | `#00866f` | `#00866f` | Secure hover on light bg |
| **rust-500** | `#ff5a1f` | `#ff5a1f` | Warnings, errors, destructive actions |
| **rust-600** | `#e94313` | `#e94313` | Critical / active error on light bg |
| **vector-500** | `#2e7fd8` | `#2e7fd8` | Charts, vector search visualization |
| **ink-950** | `#050607` | `#050607` | Page background (Blacksite dark) |
| **ink-900** | `#111316` | `#111316` | Card / surface dark |
| **ink-800** | `#22252a` | `#22252a` | Raised surface |
| **ink-500** | `#746f66` | `#746f66` | Muted text |
| **ink-300** | `#bcb5a6` | `#bcb5a6` | Subtle / placeholder text |
| **ink-50** | `#f7f6f0` | `#f7f6f0` | Foreground text on dark |

### Semantic roles

| Token | Dark (Blacksite) | Light |
|-------|-----------------|-------|
| `--primary` | `memory-400` `#d7ff3f` | `memory-500` `#bde800` |
| `--secondary` | `secure-500` `#00a88a` | `secure-600` `#00866f` |
| `--background` | `ink-950` `#050607` | `#f7f5ed` |
| `--foreground` | `ink-50` `#f7f6f0` | `#11100d` |
| `--destructive` | `rust-500` `#ff5a1f` | `rust-600` `#e94313` |
| `--ring` / focus | `memory-400` | `memory-500` |

---

## Logo

### Variants

| File | Background | Icon color | Use when |
|------|-----------|------------|----------|
| `logo/logo-png/logo-bgdark-icon-white.png` | Ink-950 dark | White | Dark UI, README dark mode |
| `logo/logo-svg/logo-bgwhite-icondark.svg` | White | Ink dark | Docs, light sections, print |
| `logo/logo-svg/logo-bgwhite-icondark-sansfond copy.svg` | Transparent | Ink dark | Overlays on light bg |
| `logo/logo-svg/logo-bgwhite-iconwhite-sansfond.svg` | Transparent | White | Overlays on dark bg |
| `logo/logo-svg/logo-monochrome-inverted.svg` | Black | White | Single-color print, emboss |
| `logo/logo-png/logo-bgwhite-avec-texte-icon-dark.png` | White | Dark + text | Full lockup, marketing |
| `logo/logo-png/logo-monochrome-inverted.png` | Black | White | Single-color print |

### Rules

- Minimum clear space: **½ × logo height** on all sides.
- Never recolor, skew, rotate, or apply drop shadows.
- Never place the dark icon on a dark background or the white icon on a white background.
- Prefer the **SVG** variants for all digital usage (they scale without loss).

---

## Icons

All 15 icons live in `icons/`. They are 840 × 840 px square SVGs using **`memory-500` (`#bde800`)** as fill — the brand's primary signal color.

| File | Represents |
|------|-----------|
| `gettingstarted.svg` | Quick-start / Getting started |
| `installation.svg` | Installation |
| `features.svg` | Features list |
| `documentation.svg` | Documentation / book |
| `security.svg` | Security & privacy |
| `community.svg` | Community |
| `contributing.svg` | Contributing / pencil |
| `license.svg` | License / badge |
| `contents.svg` | Table of contents |
| `tick.svg` | Checkmark / done |
| `cloud.svg` | Cloud / sync |
| `docker.svg` | Docker |
| `apple.svg` | macOS / Apple |
| `linux.svg` | Linux |
| `windows.svg` | Windows |

### Using icons in a GitHub README

Embed inline with a fixed width so GitHub renders them consistently:

```md
<img src="basemyai-branding/icons/gettingstarted.svg" width="24" height="24" alt="" />
```

For a section heading row:

```md
## <img src="basemyai-branding/icons/installation.svg" width="28" height="28" alt="" /> Installation
```

### Adapting for dark/light contexts

The icons are filled with `#bde800`. This works on both Blacksite dark (`ink-950`) and on white/light backgrounds. If you need a white version for a fully dark embed, replace the fill in a copy:

```
sed 's/#bde800/#ffffff/g' icons/security.svg > icons/security-white.svg
```

---

## Social Icons

Eight icons in `social/`, sized to platform-standard aspect ratios and filled with **`#AEB8CA`** (neutral blue-grey). They are intentionally desaturated so they never compete with the primary brand color.

| File | Platform |
|------|---------|
| `github.svg` | GitHub |
| `discord.svg` | Discord |
| `x.svg` | X (Twitter) |
| `linkedin.svg` | LinkedIn |
| `youtube.svg` | YouTube |
| `dev.svg` | Dev.to |
| `blog.svg` | Blog |
| `stack-overflow.svg` | Stack Overflow |

---

## Feature Images

Five PNG screenshots / illustrations in `img/`. Use them in docs or landing pages to illustrate core capabilities.

| File | Caption |
|------|--------|
| `basemyai-memory-engine.png` | Memory engine overview |
| `basemyai-memory-runtime.png` | Runtime memory graph |
| `basemyai-branch-your-agent.png` | Branch your agent |
| `basemyai-database-plugin.png` | Database plugin architecture |
| `multi-modal-database.png` | Multi-modal database layers |

---

## Tailwind CSS Integration

The file `tailwind-css/tailwind.css` is a **Tailwind v4** brand system. Import it after your base Tailwind import:

```css
@import "tailwindcss";
@import "./basemyai-branding/tailwind-css/tailwind.css";
```

This exposes all primitives as Tailwind utilities:

```html
<!-- Blacksite card -->
<div class="bg-ink-900 border border-border text-ink-50 p-6 rounded-lg">
  <span class="text-memory-400 font-semibold">Active memory</span>
  <p class="text-muted-foreground">64 vectors · 12 MB</p>
</div>

<!-- Primary CTA -->
<button class="bg-primary text-primary-foreground hover:bg-primary-hover px-4 py-2 rounded memory-glow">
  Start engine
</button>

<!-- Secure status badge -->
<span class="bg-secure-500/10 text-secure-400 px-2 py-0.5 rounded-full text-xs">
  Encrypted
</span>
```

### Theme modes

The default `:root` applies the **Blacksite dark** theme. Wrap any section in `.light` to switch:

```html
<section class="light bg-background text-foreground p-8">
  <!-- docs / marketing white section -->
</section>
```

### Component utilities

| Class | Effect |
|-------|--------|
| `.surface-blacksite` | Dark card: `bg-card` + `border-border` |
| `.surface-blacksite-raised` | Elevated surface |
| `.text-brand-gradient` | Ink-50 → ink-300 gradient text |
| `.memory-glow` | Lime ring + diffuse glow (focus highlight) |
| `.blacksite-grid` | Subtle 48 px dot/line grid background |

---

## License

Assets in this branding kit are proprietary to the BaseMyAI project. Do not redistribute, resell, or use outside of the BaseMyAI / ForgeMyAI ecosystem without explicit written permission.
