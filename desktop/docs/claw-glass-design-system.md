# Claw Glass — Desktop Design System

The single source of truth for the Claw OS desktop visual language. Every
system app and applet must conform to this. It is implemented at the toolkit
and theme layer so most of it is inherited automatically; app authors mainly
apply structure, spacing, and the documented component treatments.

## 1. Principles

- **Frosted glass, always.** App chrome and floating surfaces are translucent
  and blurred (`is_frosted` on by default). Depth comes from the compositor's
  dual-Kawase blur + layered drop shadows, not from heavy borders.
- **One accent: Claw brand blue `#005CFE`.** Used for primary actions,
  selection, focus rings, active states, and brand moments. Do not introduce
  other accent hues per-app.
- **Bright, never flat-gray, never dead-charcoal.** Light is a cool blue-white
  glass; dark is a deep navy-blue glass. Surfaces always carry a faint blue
  cast — no neutral gray fields, no pure `#000`/`#1E1E1E` charcoal.
- **Quiet structure, loud content.** Flatten dense legacy chrome (old nav-bar
  sidebars, boxed setting rows) into airy grouped glass cards with generous
  spacing.

## 2. Color

Defined in `desktop/theme/src/model/light.ron` and `dark.ron` (palette seeds);
surfaces are derived in `desktop/toolkit/cosmic-theme/src/model/theme.rs`.

| Token            | Light       | Dark        | Use                          |
|------------------|-------------|-------------|------------------------------|
| accent (blue)    | `#005CFE`   | `#005CFE`   | primary / selection / focus  |
| red (destructive)| `#FF3B30`   | `#FF453A`   | destructive                  |
| green (success)  | `#34C759`   | `#32D74B`   | success                      |
| yellow (warning) | `#FFCC00`   | `#FFD60A`   | warning                      |
| neutral_1 (base) | `#FCFDFF`   | `#05070D`   | window base                  |
| surfaces         | blue-white  | navy glass  | derived blue-tinted steps    |

Surface seeds are blue-tinted at their original luminance, so contrast ratios
are preserved while the whole UI reads as glass, not gray.

## 3. Typography

- **UI:** Inter — headings 600/650 with slightly tight tracking; body 400/450;
  labels 500.
- **Code / terminal:** JetBrains Mono.
- Establish clear hierarchy (title › section › body › caption); prefer weight
  and size contrast over color contrast for hierarchy.

## 4. Shape & elevation

Corner radii (`cosmic-theme/src/model/corner.rs`, macOS-continuous):
`xs=4, s=6, m=10, l=16, xl=100(pill)`.

- Large surfaces / cards: `radius_l` (16).
- Controls (buttons, inputs): `radius_m` (10).
- Chips / search / toggles: `radius_xl` (pill).
- Borders: 1px blue-tinted translucent hairlines only; lean on blur + shadow
  for separation, not boxes.

## 5. Components

- **Buttons:** primary = filled brand-blue with subtle translucency; secondary
  = glass + hairline; destructive = system red. Pill or `radius_m`.
- **Text inputs:** rounded `radius_m`, glass fill, brand-blue focus ring.
- **Cards / grouped lists:** frosted `radius_l` containers, hairline dividers,
  generous padding.
- **Tabs / segmented:** glass track, brand-blue active segment.
- **Selection / list rows:** brand-blue translucent highlight, never gray.
- **Popovers / applets:** frosted `radius_l`, soft shadow, no opaque gray.

## 6. Implementation map (foundation)

- Palette: `desktop/theme/src/model/{light,dark}.ron`
- Widget styles: `desktop/toolkit/src/theme/style/*.rs`
  (button, text_input, segmented_button, menu_bar, dropdown, tooltip)
- Per-widget: `desktop/toolkit/src/widget/*/style.rs` (card, button, …)
- Radii: `desktop/toolkit/cosmic-theme/src/model/corner.rs`

Changing these propagates to every cosmic app automatically.

## 7. MANDATORY per-app design research (before coding any app)

For EVERY app redesign, first gather and copy from the best real-world designs:

1. **Search** the web for the highest like/view reference designs for that app
   category (Dribbble / Behance / Mobbin / award galleries; sort by popularity).
2. **Download** the top 2–4 reference shots into
   `…/session-state/<id>/files/refs/<app>/`.
3. **Analyze** each into `files/refs/<app>/analysis.md`: layout/structure,
   color palette (hex), typography (family/weight/size/hierarchy), shape
   language (radii/borders/shadow), component treatments, motion/depth cues,
   plus anything else relevant.
4. **Reconcile** with Claw Glass — keep brand blue + frosted + bright/no-gray,
   but copy the reference's structure, spacing, hierarchy, and detail quality.
5. **Implement** to match the analyzed reference as closely as libcosmic/iced
   allows.
