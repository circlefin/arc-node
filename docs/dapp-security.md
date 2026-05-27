# DApp Security Notes

Arc is EVM-compatible, so the usual browser and wallet security rules still
apply to DApps built on top of it. This page collects small implementation
notes for application developers.

For vulnerabilities in Arc itself, use the private reporting process in
[SECURITY.md](../SECURITY.md).

## Render On-chain SVG Metadata Safely

Some ERC-721 contracts return metadata from `tokenURI()` with an inline SVG
image, commonly as a `data:image/svg+xml;base64,...` URI. Treat that SVG as
untrusted input. Any wallet or contract that can influence the metadata can
also influence the SVG contents.

Do not decode the SVG and inject it into the page:

```tsx
// Unsafe: the decoded SVG becomes trusted page markup.
<div dangerouslySetInnerHTML={{ __html: atob(metadata.image.split(",")[1]) }} />
```

Render the data URI as an image instead:

```tsx
// React / Next.js
<img src={metadata.image} alt={metadata.name ?? "Token image"} />
```

```js
// Vanilla JavaScript
const img = document.createElement("img");
img.src = metadata.image;
img.alt = metadata.name || "Token image";
container.replaceChildren(img);
```

When SVG is loaded through an image element, the browser treats it as image
content instead of executing it as part of the parent document. Avoid
`innerHTML`, `dangerouslySetInnerHTML`, `DOMParser` followed by DOM insertion,
or any equivalent path that turns untrusted SVG into live page markup.

If a DApp needs interactive SVG content, render it in a sandboxed `iframe`
instead of the main document:

```html
<iframe sandbox srcdoc="<!-- sanitized SVG document goes here -->"></iframe>
```

Only add sandbox permissions that the feature truly needs, and avoid combining
`allow-scripts` with `allow-same-origin` for untrusted content.
