// Trusted measurement program for a deck page (ADP-006, DOMAIN.md 10.4 B). It runs through
// Runtime.evaluate in the isolated render page; the page's own scripts are disabled and its CSP
// forbids any, so nothing in the document can reach or alter this program or its result.
//
// It reports what the browser laid out, not what the markup claims: every figure below is read from
// computed style and layout boxes. The server compares the report with the source slide and with
// the quality limits in config/outputs.yaml; this program decides nothing.
(async (limits) => {
  const tolerance = limits.overflow_tolerance_px;
  const parseColor = (value) => {
    const match = /^rgba?\(([^)]+)\)$/.exec(value || '');
    if (!match) return null;
    const parts = match[1].split(/[ ,\/]+/).filter(Boolean).map(Number);
    if (parts.length < 3 || parts.some((part) => Number.isNaN(part))) return null;
    return { r: parts[0], g: parts[1], b: parts[2], a: parts.length > 3 ? parts[3] : 1 };
  };
  const channel = (value) => {
    const v = value / 255;
    return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  };
  const luminance = (c) => 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
  const ratio = (a, b) => {
    const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
    return (hi + 0.05) / (lo + 0.05);
  };
  const over = (top, bottom) => ({
    r: top.r * top.a + bottom.r * (1 - top.a),
    g: top.g * top.a + bottom.g * (1 - top.a),
    b: top.b * top.a + bottom.b * (1 - top.a),
    a: 1,
  });
  // The colour a piece of text is painted on. A background image or gradient anywhere up the chain
  // makes the colour impossible to establish from style, and the check then reports it as such
  // instead of guessing.
  const backgroundOf = (element) => {
    const layers = [];
    for (let node = element; node; node = node.parentElement) {
      const style = getComputedStyle(node);
      if (style.backgroundImage && style.backgroundImage !== 'none') return null;
      const color = parseColor(style.backgroundColor);
      if (color && color.a > 0) {
        layers.push(color);
        if (color.a >= 1) break;
      }
    }
    let base = { r: 255, g: 255, b: 255, a: 1 };
    for (let i = layers.length - 1; i >= 0; i -= 1) base = over(layers[i], base);
    return base;
  };
  const visible = (element) => {
    const style = getComputedStyle(element);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
    const box = element.getBoundingClientRect();
    return box.width > 0 && box.height > 0;
  };
  const snippet = (text) => text.replace(/\s+/g, ' ').trim().slice(0, 60);
  const clipping = new Set(['hidden', 'auto', 'scroll', 'clip']);

  if (document.fonts && document.fonts.ready) await document.fonts.ready;
  await Promise.all(Array.from(document.images).map((image) => image.decode().catch(() => undefined)));

  const roots = Array.from(document.querySelectorAll('[data-slide]'));
  const slides = roots.map((root) => {
    const rootBox = root.getBoundingClientRect();
    const everything = [root, ...root.querySelectorAll('*')].filter(visible);

    // Overflow: content clipped by a box, or painted beyond the page.
    const clipped = [];
    const outside = [];
    for (const element of everything) {
      const style = getComputedStyle(element);
      if (
        (clipping.has(style.overflowY) || element === root) &&
        element.scrollHeight > element.clientHeight + tolerance
      ) {
        clipped.push(element);
      }
      if (clipping.has(style.overflowX) && element.scrollWidth > element.clientWidth + tolerance) {
        clipped.push(element);
      }
      if (element !== root) {
        const box = element.getBoundingClientRect();
        if (
          box.bottom > rootBox.bottom + tolerance ||
          box.right > rootBox.right + tolerance ||
          box.left < rootBox.left - tolerance ||
          box.top < rootBox.top - tolerance
        ) {
          outside.push(element);
        }
      }
    }
    const offenders = Array.from(new Set([...clipped, ...outside]));

    // Contrast: every element that directly holds text.
    const failures = [];
    let checked = 0;
    let unverifiable = 0;
    for (const element of everything) {
      const own = Array.from(element.childNodes)
        .filter((node) => node.nodeType === Node.TEXT_NODE)
        .map((node) => node.textContent)
        .join('');
      if (!own.trim()) continue;
      const style = getComputedStyle(element);
      const foreground = parseColor(style.color);
      const background = backgroundOf(element);
      if (!foreground || !background) {
        unverifiable += 1;
        continue;
      }
      const painted = foreground.a < 1 ? over(foreground, background) : foreground;
      const size = parseFloat(style.fontSize);
      const bold = Number(style.fontWeight) >= 700;
      const large = size >= 24 || (bold && size >= 18.66);
      const needed = large ? limits.min_contrast_large : limits.min_contrast_text;
      const got = ratio(painted, background);
      checked += 1;
      if (got < needed) failures.push({ text: snippet(own), ratio: Math.round(got * 100) / 100, needed });
    }

    // Accessible names for non-text content.
    const missingAlt = [];
    const named = (element) => {
      if (element.getAttribute('aria-hidden') === 'true' || element.getAttribute('role') === 'presentation') return true;
      if (element.hasAttribute('aria-label') && element.getAttribute('aria-label').trim()) return true;
      if (element.hasAttribute('aria-labelledby')) return true;
      if (element.tagName.toLowerCase() === 'img') return Boolean((element.getAttribute('alt') || '').trim());
      if (element.tagName.toLowerCase() === 'svg') {
        const title = element.querySelector(':scope > title');
        return Boolean(title && title.textContent.trim());
      }
      return false;
    };
    for (const element of root.querySelectorAll('img, svg, canvas, video, audio, object, embed')) {
      if (element.closest('svg') && element.tagName.toLowerCase() !== 'svg') continue;
      if (element.parentElement && element.parentElement.closest('svg')) continue;
      if (!named(element)) missingAlt.push({ tag: element.tagName.toLowerCase() });
    }

    const text = root.innerText || '';
    const figures = root.querySelectorAll('img, svg, canvas, video').length;
    return {
      index: root.getAttribute('data-slide'),
      width: Math.round(rootBox.width),
      height: Math.round(rootBox.height),
      text,
      text_chars: text.replace(/\s+/g, '').length,
      figures,
      overflow: offenders.length
        ? { count: offenders.length, first: snippet(offenders[0].innerText || offenders[0].tagName) }
        : { count: 0, first: '' },
      contrast: { checked, unverifiable, failures },
      missing_alt: missingAlt,
    };
  });
  return {
    slides,
    document_text: document.body ? document.body.innerText : '',
    slide_roots: roots.length,
    scripts: document.scripts.length,
  };
})
