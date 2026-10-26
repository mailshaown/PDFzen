const { invoke } = window.__TAURI__.core;
const { open: openDialog, save: saveDialog } = window.__TAURI__.dialog;

pdfjsLib.GlobalWorkerOptions.workerSrc = "/vendor/pdf.worker.min.js";

/* ------------------------------------------------------------------ *
 * state
 * ------------------------------------------------------------------ */

const DEFAULT_SETTINGS = {
  theme: "auto",
  flatten: false,
  removeMetadata: false,
  pageNumbers: false,
  watermark: "",
  watermarkOpacity: 0.12,
  watermarkAngle: 45,
  info: { title: "", author: "", subject: "", keywords: "" },
};

const THEME_MODES = ["auto", "light", "dark"];
const SETTINGS_KEY = "pdfzen-settings";
const LEGACY_SETTINGS_KEY = "pdf-editor-settings";
const systemThemeMedia = window.matchMedia("(prefers-color-scheme: dark)");

function defaultSecurity() {
  return {
    enabled: false,
    userPassword: "",
    ownerPassword: "",
    allowPrint: true,
    allowCopy: true,
    allowModify: false,
    allowAnnotate: false,
    allowAssemble: false,
  };
}

const state = {
  docs: new Map(), // id -> { base64, pdf, name, outline }
  pages: [], // { docId, index, rotation, intrinsic, annos[], crop, searchHits[], dim }
  current: 0,
  scale: 1.2,
  tool: "select",
  color: "#ffd400",
  colorTouched: false,
  strokeWidth: 2,
  fontSize: 12,
  search: { query: "", matches: [], index: -1 },
  path: null,
  history: [],
  future: [],
  settings: JSON.parse(JSON.stringify(DEFAULT_SETTINGS)),
  security: defaultSecurity(),
  pendingImage: null,
  activeTab: "thumbs",
  redactWarned: false,
};

const recs = []; // per page DOM record
const visible = new Set();
let drag = null;
let pendingSelection = null;
let commentTarget = null;
let selectionTimer = 0;

/* ------------------------------------------------------------------ *
 * dom
 * ------------------------------------------------------------------ */

const els = {
  workspace: document.querySelector(".workspace"),
  sidebar: document.querySelector(".sidebar"),
  sidebarResizer: document.getElementById("sidebar-resizer"),
  viewer: document.getElementById("viewer"),
  pages: document.getElementById("pages"),
  empty: document.getElementById("empty"),
  thumbs: document.getElementById("thumbs"),
  outline: document.getElementById("outline"),
  comments: document.getElementById("comments"),
  zoomLabel: document.getElementById("zoom-label"),
  search: document.getElementById("search"),
  searchLabel: document.getElementById("search-label"),
  statusPage: document.getElementById("status-page"),
  statusTool: document.getElementById("status-tool"),
  statusHint: document.getElementById("status-hint"),
  color: document.getElementById("color"),
  width: document.getElementById("width"),
  stampLabel: document.getElementById("stamp-label"),
  exportMenu: document.getElementById("export-menu"),
  themeMenu: document.getElementById("theme-menu"),
  bookmarkTools: document.getElementById("bookmark-tools"),
  bookmarkTitle: document.getElementById("bookmark-title"),
  commentTools: document.getElementById("comment-tools"),
  commentDraft: document.getElementById("comment-draft"),
  selectionMenu: document.getElementById("selection-menu"),
  commentPopover: document.getElementById("comment-popover"),
  commentPopoverText: document.getElementById("comment-popover-text"),
  commentRemove: document.getElementById("btn-comment-remove"),
  modalSign: document.getElementById("modal-sign"),
  modalOptions: document.getElementById("modal-options"),
  modalPassword: document.getElementById("modal-password"),
  passwordInput: document.getElementById("password-input"),
  passwordError: document.getElementById("password-error"),
  passwordMessage: document.getElementById("password-message"),
  signPad: document.getElementById("sign-pad"),
  signPreview: document.getElementById("sign-preview"),
  signName: document.getElementById("sign-name"),
  signFont: document.getElementById("sign-font"),
  signColor: document.getElementById("sign-color"),
  printArea: document.getElementById("print-area"),
};

/* ------------------------------------------------------------------ *
 * helpers
 * ------------------------------------------------------------------ */

const clamp = (v, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, v));
const uid = () => Math.random().toString(36).slice(2) + Date.now().toString(36);

const SIDEBAR_DEFAULT_WIDTH = 224;
const SIDEBAR_MIN_WIDTH = 176;
let sidebarResize = null;

function sidebarMaxWidth() {
  return Math.max(120, Math.floor(window.innerWidth * 0.5));
}

function setSidebarWidth(width) {
  const max = sidebarMaxWidth();
  const min = Math.min(SIDEBAR_MIN_WIDTH, max);
  const next = Math.round(clamp(width, min, max));
  els.workspace?.style.setProperty("--sidebar-width", `${next}px`);
  if (els.sidebarResizer) {
    els.sidebarResizer.setAttribute("aria-valuemin", String(min));
    els.sidebarResizer.setAttribute("aria-valuemax", String(max));
    els.sidebarResizer.setAttribute("aria-valuenow", String(next));
  }
  return next;
}

function thumbnailWidth() {
  const width = els.sidebar?.clientWidth || SIDEBAR_DEFAULT_WIDTH;
  return Math.round(clamp(width - 26, 108, 320));
}

function refreshThumbWidths() {
  if (!state.pages.length) return;
  els.thumbs.querySelectorAll(".thumb canvas").forEach((canvas, i) => {
    renderThumb(i, canvas);
  });
}

let thumbRefreshTimer = 0;

function scheduleThumbRefresh() {
  clearTimeout(thumbRefreshTimer);
  thumbRefreshTimer = setTimeout(refreshThumbWidths, 140);
}

function beginSidebarResize(ev) {
  if (ev.button !== 0) return;
  ev.preventDefault();
  const startWidth = els.sidebar.getBoundingClientRect().width;
  sidebarResize = { pointerId: ev.pointerId, startX: ev.clientX, startWidth };
  els.sidebarResizer.classList.add("dragging");
  document.body.classList.add("resizing-sidebar");
  try {
    els.sidebarResizer.setPointerCapture(ev.pointerId);
  } catch {
    /* ignore */
  }
}

function moveSidebarResize(ev) {
  if (!sidebarResize || ev.pointerId !== sidebarResize.pointerId) return;
  ev.preventDefault();
  setSidebarWidth(sidebarResize.startWidth + ev.clientX - sidebarResize.startX);
}

function endSidebarResize(ev) {
  if (!sidebarResize || ev.pointerId !== sidebarResize.pointerId) return;
  sidebarResize = null;
  els.sidebarResizer.classList.remove("dragging");
  document.body.classList.remove("resizing-sidebar");
  refreshThumbWidths();
}

const KIND_LABELS = {
  highlight: "Highlight",
  underline: "Underline",
  strikeout: "Strikeout",
  squiggly: "Squiggly",
  ink: "Ink",
  square: "Rectangle",
  text: "Text",
  note: "Sticky note",
  stamp: "Stamp",
  image: "Signature",
  redact: "Black box",
};

const MARKUP_KINDS = new Set(["highlight", "underline", "strikeout", "squiggly"]);
const COLOR_KINDS = new Set(["highlight", "underline", "strikeout", "squiggly", "ink", "square", "text", "note", "stamp"]);

function base64ToBytes(b64) {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

function bytesToBase64(bytes) {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

function hexToRgb01(hex) {
  const clean = (hex || "#ffd400").replace("#", "");
  const full =
    clean.length === 3
      ? clean
          .split("")
          .map((c) => c + c)
          .join("")
      : clean;
  const num = parseInt(full, 16);
  return [((num >> 16) & 255) / 255, ((num >> 8) & 255) / 255, (num & 255) / 255];
}

function rgba(hex, alpha) {
  const [r, g, b] = hexToRgb01(hex);
  return `rgba(${Math.round(r * 255)},${Math.round(g * 255)},${Math.round(b * 255)},${alpha})`;
}

function normRect(a, b) {
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  const w = Math.abs(a.x - b.x);
  const h = Math.abs(a.y - b.y);
  return [x, y, w, h];
}

function setHint(text) {
  els.statusHint.textContent = text;
}

function joinPath(dir, name) {
  return `${dir.replace(/[\\/]+$/, "")}\\${name}`;
}

function docBaseName() {
  if (!state.path) return "document";
  const file = state.path.split(/[\\/]/).pop() || "document";
  return file.replace(/\.pdf$/i, "");
}

function sanitizeName(name) {
  return String(name).replace(/[^A-Za-z0-9._-]+/g, "_").slice(0, 60) || "file";
}

/* ------------------------------------------------------------------ *
 * documents & page metadata
 * ------------------------------------------------------------------ */

async function pageInfo(entry) {
  const doc = state.docs.get(entry.docId);
  if (!doc) throw new Error("document not loaded");
  const page = await doc.pdf.getPage(entry.index + 1);
  if (entry.intrinsic == null) entry.intrinsic = page.rotate || 0;
  const total = (((entry.intrinsic + entry.rotation) % 360) + 360) % 360;
  return { page, total };
}

function invalidate(entry) {
  entry.dim = null;
}

/* ------------------------------------------------------------------ *
 * page dom + painting
 * ------------------------------------------------------------------ */

function createPageDom() {
  els.pages.innerHTML = "";
  recs.length = 0;
  visible.clear();
  const observer = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        const i = Number(e.target.dataset.page);
        if (e.isIntersecting) {
          visible.add(i);
          paintIfStale(i);
        } else {
          visible.delete(i);
        }
      }
    },
    { root: els.viewer, rootMargin: "400px 0px" },
  );
  window.__pageObserver?.disconnect();
  window.__pageObserver = observer;

  state.pages.forEach((entry, i) => {
    const wrap = document.createElement("div");
    wrap.className = "page-wrap";
    wrap.dataset.page = String(i);

    const label = document.createElement("div");
    label.className = "page-label";
    label.textContent = String(i + 1);

    const surface = document.createElement("div");
    surface.className = "page-surface";

    const canvas = document.createElement("canvas");
    canvas.className = "page-canvas";

    const textWrap = document.createElement("div");
    textWrap.className = "page-text";
    const textLayer = document.createElement("div");
    textLayer.className = "textLayer";
    textWrap.appendChild(textLayer);

    const overlay = document.createElement("canvas");
    overlay.className = "page-overlay";

    surface.appendChild(canvas);
    surface.appendChild(textWrap);
    surface.appendChild(overlay);
    wrap.appendChild(label);
    wrap.appendChild(surface);
    els.pages.appendChild(wrap);

    const rec = {
      wrap,
      surface,
      canvas,
      textWrap,
      textLayer,
      overlay,
      ctx: canvas.getContext("2d"),
      octx: overlay.getContext("2d"),
      w: 0,
      h: 0,
      dirty: true,
      painting: false,
      paintedScale: 0,
      textScale: -1,
      textTotal: -1,
      textPainting: false,
      textPaintPromise: null,
    };
    recs[i] = rec;

    overlay.addEventListener("pointerdown", (ev) => onPointerDown(ev, i));
    overlay.addEventListener("pointermove", onPointerMove);
    overlay.addEventListener("pointerup", onPointerUp);
    overlay.addEventListener("pointercancel", () => {
      drag = null;
    });

    observer.observe(wrap);
  });
}

async function relayout() {
  await Promise.all(
    state.pages.map(async (entry) => {
      if (entry.dim) return;
      const info = await pageInfo(entry);
      const vp = info.page.getViewport({ scale: 1, rotation: info.total });
      entry.dim = { w: vp.width, h: vp.height, total: info.total };
    }),
  );

  state.pages.forEach((entry, i) => {
    const rec = recs[i];
    if (!rec) return;
    const w = entry.dim.w * state.scale;
    const h = entry.dim.h * state.scale;
    rec.w = w;
    rec.h = h;
    rec.surface.style.width = `${w}px`;
    rec.surface.style.height = `${h}px`;
    rec.canvas.style.width = `${w}px`;
    rec.canvas.style.height = `${h}px`;
    rec.textWrap.style.width = `${w}px`;
    rec.textWrap.style.height = `${h}px`;
    rec.textLayer.style.width = `${w}px`;
    rec.textLayer.style.height = `${h}px`;
    rec.dirty = true;
    rec.textScale = -1;
    drawOverlay(i);
  });

  els.zoomLabel.textContent = `${Math.round(state.scale * 100)}%`;
  // The IntersectionObserver can fire before relayout has assigned page sizes,
  // in which case paintIfStale() returns early and never retries. Seed the
  // visible set from actual geometry after every layout pass.
  for (let i = 0; i < state.pages.length; i++) {
    if (pageInViewport(i)) visible.add(i);
  }
  paintVisible();
  updateStatus();
}

function pageInViewport(i) {
  const rec = recs[i];
  if (!rec) return false;
  const v = els.viewer.getBoundingClientRect();
  const r = rec.wrap.getBoundingClientRect();
  return r.bottom > v.top - 400 && r.top < v.bottom + 400;
}

async function paintIfStale(i) {
  const rec = recs[i];
  const entry = state.pages[i];
  if (!rec || !entry) return;
  if (!rec.w || !rec.h) return;
  if (rec.painting) return;
  if (!rec.dirty && rec.paintedScale === state.scale) {
    paintTextLayer(i);
    return;
  }
  rec.painting = true;
  const scale = state.scale;
  const total = entry.dim.total;
  let retry = false;
  try {
    const info = await pageInfo(entry);
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const viewport = info.page.getViewport({
      scale: scale * dpr,
      rotation: info.total,
    });
    rec.canvas.width = Math.max(1, Math.floor(viewport.width));
    rec.canvas.height = Math.max(1, Math.floor(viewport.height));
    rec.canvas.style.width = `${rec.w}px`;
    rec.canvas.style.height = `${rec.h}px`;
    await info.page.render({ canvasContext: rec.ctx, viewport }).promise;
    rec.dirty = false;
    rec.paintedScale = scale;
    drawOverlay(i);
    paintTextLayer(i);
    retry = state.scale !== scale || entry.dim.total !== total;
  } catch (err) {
    console.error("render failed", err);
  } finally {
    rec.painting = false;
    if (retry) {
      rec.dirty = true;
      rec.textScale = -1;
      requestAnimationFrame(() => paintIfStale(i));
    }
  }
}

function paintTextLayer(i) {
  const rec = recs[i];
  const entry = state.pages[i];
  if (!rec || !entry || !entry.dim) return Promise.resolve();
  if (rec.textScale === state.scale && rec.textTotal === entry.dim.total) {
    return Promise.resolve();
  }
  if (rec.textPaintPromise) return rec.textPaintPromise;

  rec.textPainting = true;
  rec.textPaintPromise = (async () => {
    const scale = state.scale;
    const total = entry.dim.total;
    let retry = false;
    try {
      const info = await pageInfo(entry);
      const viewport = info.page.getViewport({ scale, rotation: info.total });
      rec.textLayer.style.width = `${rec.w}px`;
      rec.textLayer.style.height = `${rec.h}px`;
      rec.textLayer.style.setProperty("--scale-factor", String(scale));
      rec.textLayer.textContent = "";
      const content = await info.page.getTextContent();
      if (typeof pdfjsLib.renderTextLayer === "function") {
        const task = pdfjsLib.renderTextLayer({
          textContentSource: content,
          textContent: content,
          container: rec.textLayer,
          viewport,
          textDivs: [],
        });
        if (task?.promise) await task.promise;
        rec.textScale = scale;
        rec.textTotal = total;
      }
      retry = state.scale !== scale || entry.dim.total !== total;
    } catch (err) {
      console.warn("text layer failed", err);
    } finally {
      rec.textPainting = false;
      rec.textPaintPromise = null;
      if (retry) {
        rec.textScale = -1;
        requestAnimationFrame(() => paintTextLayer(i));
      }
    }
  })();
  return rec.textPaintPromise;
}

function paintVisible() {
  for (const i of visible) paintIfStale(i);
}

const imageCache = new Map();

function imageFor(data, mime = "png") {
  const key = `${mime}:${data.slice(0, 32)}:${data.length}`;
  let img = imageCache.get(key);
  if (!img) {
    img = new Image();
    img.onload = () => {
      recs.forEach((rec, idx) => {
        if (rec && state.pages[idx]?.annos.some((a) => a.image === data)) drawOverlay(idx);
      });
    };
    img.src = `data:image/${mime};base64,${data}`;
    imageCache.set(key, img);
  }
  return img;
}

function drawOverlay(i) {
  const rec = recs[i];
  const entry = state.pages[i];
  if (!rec || !entry) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const w = rec.w || 1;
  const h = rec.h || 1;

  if (rec.overlay.width !== Math.floor(w * dpr) || rec.overlay.height !== Math.floor(h * dpr)) {
    rec.overlay.width = Math.floor(w * dpr);
    rec.overlay.height = Math.floor(h * dpr);
  }
  rec.overlay.style.width = `${w}px`;
  rec.overlay.style.height = `${h}px`;

  const ctx = rec.octx;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const hitAlpha = (match) =>
    state.search.index >= 0 && state.search.matches[state.search.index] === match ? 0.55 : 0.32;
  const pageMatches = state.search.matches.filter((m) => m.page === i);
  for (const match of pageMatches) {
    for (const r of match.rects) {
      ctx.fillStyle = `rgba(255, 214, 10, ${hitAlpha(match)})`;
      ctx.fillRect(r[0] * w, r[1] * h, r[2] * w, r[3] * h);
    }
  }

  drawAnnotations(ctx, entry, w, h);

  if (entry.crop) drawCropShade(ctx, entry.crop, w, h);

  if (drag && drag.page === i) {
    drawDragPreview(ctx, drag, w, h);
  }

  if (i === state.current && state.pages.length > 1) {
    ctx.save();
    ctx.strokeStyle = "rgba(37,99,235,0.55)";
    ctx.lineWidth = 2;
    ctx.strokeRect(1, 1, w - 2, h - 2);
    ctx.restore();
  }
}

function drawCropShade(ctx, crop, w, h) {
  const [x0, y0, x1, y1] = crop;
  ctx.save();
  ctx.fillStyle = "rgba(15,17,20,0.48)";
  ctx.fillRect(0, 0, w, Math.max(0, y0 * h));
  ctx.fillRect(0, y1 * h, w, Math.max(0, (1 - y1) * h));
  ctx.fillRect(0, y0 * h, Math.max(0, x0 * w), Math.max(0, (y1 - y0) * h));
  ctx.fillRect(x1 * w, y0 * h, Math.max(0, (1 - x1) * w), Math.max(0, (y1 - y0) * h));
  ctx.strokeStyle = "rgba(77,139,255,0.95)";
  ctx.setLineDash([7, 4]);
  ctx.lineWidth = 1.5;
  ctx.strokeRect(x0 * w, y0 * h, (x1 - x0) * w, (y1 - y0) * h);
  ctx.restore();
}

function drawDragPreview(ctx, d, w, h) {
  ctx.save();
  ctx.setLineDash([4, 3]);
  ctx.lineWidth = 1.5;
  ctx.strokeStyle = "rgba(37,99,235,0.9)";
  const c = state.color;
  const [x, y, rw, rh] = normRect(d.start, d.current);

  if (d.kind === "ink") {
    if (d.points.length > 1) {
      ctx.setLineDash([]);
      ctx.strokeStyle = c;
      ctx.lineWidth = state.strokeWidth;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.beginPath();
      d.points.forEach((p, idx) => {
        if (idx === 0) ctx.moveTo(p.x * w, p.y * h);
        else ctx.lineTo(p.x * w, p.y * h);
      });
      ctx.stroke();
    }
    ctx.restore();
    return;
  }

  if (d.kind === "crop") {
    drawCropShade(ctx, [x, y, x + rw, y + rh], w, h);
    ctx.restore();
    return;
  }

  if (d.kind === "highlight") {
    ctx.fillStyle = rgba(c, 0.3);
    ctx.fillRect(x * w, y * h, rw * w, rh * h);
  } else if (d.kind === "redact") {
    ctx.fillStyle = "rgba(0,0,0,0.8)";
    ctx.fillRect(x * w, y * h, rw * w, rh * h);
  } else if (d.kind === "stamp") {
    ctx.fillStyle = rgba(c, 0.08);
    ctx.fillRect(x * w, y * h, rw * w, rh * h);
  } else if (d.kind === "image" && state.pendingImage) {
    const img = imageFor(state.pendingImage.data, "png");
    if (img.complete && img.naturalWidth) {
      ctx.globalAlpha = 0.7;
      ctx.drawImage(img, x * w, y * h, rw * w, rh * h);
      ctx.globalAlpha = 1;
    } else {
      ctx.fillStyle = rgba(c, 0.1);
      ctx.fillRect(x * w, y * h, rw * w, rh * h);
    }
  }
  ctx.strokeRect(x * w, y * h, rw * w, rh * h);
  ctx.restore();
}

function annotationRects(a) {
  if (Array.isArray(a.rects) && a.rects.length) return a.rects;
  return a.rect ? [a.rect] : [];
}

function unionRects(rects) {
  if (!rects.length) return null;
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const [x, y, rw, rh] of rects) {
    x0 = Math.min(x0, x);
    y0 = Math.min(y0, y);
    x1 = Math.max(x1, x + rw);
    y1 = Math.max(y1, y + rh);
  }
  return [x0, y0, x1 - x0, y1 - y0];
}

function drawCommentBadge(ctx, rects, w, h) {
  const box = unionRects(rects);
  if (!box) return;
  const size = Math.max(5, Math.min(12, w * 0.017));
  const cx = clamp((box[0] + box[2]) * w - size * 0.45, size, w - size);
  const cy = clamp(box[1] * h + size * 0.55, size, h - size);
  ctx.save();
  ctx.fillStyle = "#f59e0b";
  ctx.strokeStyle = "#fff";
  ctx.lineWidth = Math.max(0.7, size * 0.12);
  ctx.beginPath();
  ctx.arc(cx, cy, size * 0.55, 0, Math.PI * 2);
  ctx.fill();
  ctx.stroke();
  ctx.strokeStyle = "#fff";
  ctx.lineWidth = Math.max(0.6, size * 0.1);
  for (let k = -1; k <= 1; k++) {
    ctx.beginPath();
    ctx.moveTo(cx - size * 0.22, cy + k * size * 0.16);
    ctx.lineTo(cx + size * 0.22, cy + k * size * 0.16);
    ctx.stroke();
  }
  ctx.restore();
}

function drawAnnotations(ctx, entry, w, h) {
  for (const a of entry.annos) {
    const rects = annotationRects(a);
    if (a.kind === "highlight") {
      ctx.fillStyle = rgba(a.color, 0.35);
      for (const [x, y, rw, rh] of rects) {
        ctx.fillRect(x * w, y * h, rw * w, rh * h);
      }
      if (a.text) drawCommentBadge(ctx, rects, w, h);
    } else if (a.kind === "underline" || a.kind === "strikeout" || a.kind === "squiggly") {
      ctx.save();
      ctx.strokeStyle = a.color;
      for (const [x, y, rw, rh] of rects) {
        ctx.lineWidth = Math.max(1, Math.min(3, rh * h * 0.16));
        if (a.kind === "underline") {
          const yy = y * h + rh * h * 0.9;
          ctx.beginPath();
          ctx.moveTo(x * w, yy);
          ctx.lineTo((x + rw) * w, yy);
          ctx.stroke();
        } else if (a.kind === "strikeout") {
          const yy = y * h + rh * h * 0.5;
          ctx.beginPath();
          ctx.moveTo(x * w, yy);
          ctx.lineTo((x + rw) * w, yy);
          ctx.stroke();
        } else {
          const amp = Math.max(1.2, rh * h * 0.16);
          const step = Math.max(2, (rw * w) / 16);
          let xx = x * w;
          let up = true;
          ctx.beginPath();
          ctx.moveTo(xx, y * h + rh * h * 0.5);
          while (xx < (x + rw) * w) {
            xx = Math.min((x + rw) * w, xx + step);
            ctx.lineTo(xx, y * h + rh * h * 0.5 + (up ? amp : -amp));
            up = !up;
          }
          ctx.stroke();
        }
      }
      ctx.restore();
    } else if (a.kind === "ink") {
      if (!a.points || a.points.length < 2) continue;
      ctx.strokeStyle = a.color;
      ctx.lineWidth = Math.max(1, a.width || 2);
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.beginPath();
      a.points.forEach((p, idx) => {
        if (idx === 0) ctx.moveTo(p[0] * w, p[1] * h);
        else ctx.lineTo(p[0] * w, p[1] * h);
      });
      ctx.stroke();
    } else if (a.kind === "square") {
      const [x, y, rw, rh] = a.rect;
      ctx.strokeStyle = a.color;
      ctx.lineWidth = Math.max(1, a.width || 2);
      ctx.strokeRect(x * w, y * h, rw * w, rh * h);
    } else if (a.kind === "redact") {
      const [x, y, rw, rh] = a.rect;
      ctx.fillStyle = "#000";
      ctx.fillRect(x * w, y * h, rw * w, rh * h);
    } else if (a.kind === "text") {
      const [x, y, rw, rh] = a.rect;
      const size = (a.size || 12) * (w / (entry.dim?.w || w));
      ctx.save();
      ctx.beginPath();
      ctx.rect(x * w, y * h, rw * w, rh * h);
      ctx.clip();
      ctx.fillStyle = a.color;
      ctx.font = `${size}px Helvetica, Arial, sans-serif`;
      ctx.textBaseline = "top";
      String(a.text || "")
        .split("\n")
        .forEach((line, idx) => {
          ctx.fillText(line, x * w + 2, y * h + idx * size * 1.2);
        });
      ctx.restore();
    } else if (a.kind === "note") {
      const [x, y, rw, rh] = a.rect;
      const size = Math.max(14, Math.min(26, rw * w));
      ctx.save();
      ctx.fillStyle = a.color || "#ffd400";
      ctx.strokeStyle = "rgba(0,0,0,0.6)";
      ctx.lineWidth = 1;
      ctx.fillRect(x * w, y * h, size, size);
      ctx.strokeRect(x * w + 0.5, y * h + 0.5, size - 1, size - 1);
      ctx.fillStyle = "rgba(255,255,255,0.65)";
      ctx.beginPath();
      ctx.moveTo(x * w + size * 0.62, y * h + 1);
      ctx.lineTo(x * w + size - 1, y * h + size * 0.38);
      ctx.lineTo(x * w + size - 1, y * h + 1);
      ctx.closePath();
      ctx.fill();
      ctx.strokeStyle = "rgba(0,0,0,0.45)";
      for (let k = 0; k < 3; k++) {
        ctx.beginPath();
        ctx.moveTo(x * w + size * 0.22, y * h + size * (0.34 + k * 0.18));
        ctx.lineTo(x * w + size * 0.7, y * h + size * (0.34 + k * 0.18));
        ctx.stroke();
      }
      ctx.restore();
    } else if (a.kind === "stamp") {
      const [x, y, rw, rh] = a.rect;
      const pw = rw * w;
      const ph = rh * h;
      const label = String(a.label || "APPROVED");
      const fs = Math.max(6, Math.min(ph * 0.45, (pw * 0.86) / Math.max(1, label.length * 0.62)));
      ctx.save();
      ctx.strokeStyle = a.color;
      ctx.lineWidth = 2;
      ctx.strokeRect(x * w + 1, y * h + 1, Math.max(1, pw - 2), Math.max(1, ph - 2));
      ctx.fillStyle = a.color;
      ctx.font = `bold ${fs}px Helvetica, Arial, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(label, x * w + pw / 2, y * h + ph / 2);
      ctx.restore();
    } else if (a.kind === "image") {
      const [x, y, rw, rh] = a.rect;
      const img = imageFor(a.image, a.mime || "png");
      if (img.complete && img.naturalWidth) {
        ctx.drawImage(img, x * w, y * h, rw * w, rh * h);
      } else {
        ctx.save();
        ctx.setLineDash([4, 3]);
        ctx.strokeStyle = "rgba(37,99,235,0.7)";
        ctx.strokeRect(x * w, y * h, rw * w, rh * h);
        ctx.restore();
      }
    }
  }
}

/* ------------------------------------------------------------------ *
 * annotation interaction
 * ------------------------------------------------------------------ */

function normPoint(rec, ev) {
  const r = rec.overlay.getBoundingClientRect();
  return {
    x: clamp((ev.clientX - r.left) / r.width),
    y: clamp((ev.clientY - r.top) / r.height),
  };
}

function onPointerDown(ev, i) {
  if (ev.button !== 0) return;
  if (state.tool === "select") return;
  const rec = recs[i];
  if (!rec) return;
  const p = normPoint(rec, ev);

  if (state.tool === "text") {
    startTextEditor(i, p, "text");
    return;
  }
  if (state.tool === "note") {
    startTextEditor(i, p, "note");
    return;
  }
  if (state.tool === "erase") {
    eraseAt(i, p);
    return;
  }
  if (state.tool === "image" && !state.pendingImage) {
    setHint("Use the Sign… button to create a signature image first");
    return;
  }

  overlayCapture(rec, ev);
  drag = {
    page: i,
    kind: state.tool,
    start: p,
    current: p,
    points: [p],
  };
  drawOverlay(i);
}

function overlayCapture(rec, ev) {
  try {
    rec.overlay.setPointerCapture(ev.pointerId);
  } catch {
    /* ignore */
  }
}

function onPointerMove(ev) {
  if (!drag) return;
  const rec = recs[drag.page];
  if (!rec) return;
  const p = normPoint(rec, ev);
  drag.current = p;
  if (drag.kind === "ink") {
    const last = drag.points[drag.points.length - 1];
    if (Math.abs(last.x - p.x) + Math.abs(last.y - p.y) > 0.0008) drag.points.push(p);
  }
  drawOverlay(drag.page);
}

function onPointerUp() {
  if (!drag) return;
  const { page: i, kind, start, current, points } = drag;
  drag = null;
  const entry = state.pages[i];
  const rec = recs[i];
  if (!entry || !rec) return;

  if (kind === "ink") {
    if (points.length < 2) {
      drawOverlay(i);
      return;
    }
    pushHistory();
    entry.annos.push({
      kind: "ink",
      points: points.map((p) => [Number(p.x.toFixed(4)), Number(p.y.toFixed(4))]),
      color: state.color,
      width: state.strokeWidth,
    });
    afterAnnotationChange(i);
    return;
  }

  const rect = normRect(start, current).map((v) => Number(v.toFixed(4)));

  if (kind === "crop") {
    if (rect[2] < 0.02 || rect[3] < 0.02) {
      drawOverlay(i);
      return;
    }
    pushHistory();
    entry.crop = [
      Number(rect[0].toFixed(4)),
      Number(rect[1].toFixed(4)),
      Number((rect[0] + rect[2]).toFixed(4)),
      Number((rect[1] + rect[3]).toFixed(4)),
    ];
    setTool("select");
    afterAnnotationChange(i);
    updateStatus();
    setHint("Crop applied to this page. It takes effect when saved.");
    return;
  }

  if (rect[2] < 0.004 || rect[3] < 0.004) {
    drawOverlay(i);
    return;
  }

  if (kind === "image") {
    if (!state.pendingImage) {
      drawOverlay(i);
      return;
    }
    pushHistory();
    entry.annos.push({
      kind: "image",
      rect,
      image: state.pendingImage.data,
      mime: "png",
      label: "Signature",
    });
    state.pendingImage = null;
    setTool("select");
    afterAnnotationChange(i);
    setHint("Signature placed. Save to embed it.");
    return;
  }

  if (kind === "stamp") {
    pushHistory();
    entry.annos.push({
      kind: "stamp",
      rect,
      label: els.stampLabel.value || "APPROVED",
      color: state.color,
    });
    afterAnnotationChange(i);
    return;
  }

  if (kind === "redact") {
    pushHistory();
    entry.annos.push({
      kind: "redact",
      rect,
      color: "#000000",
      width: 0,
    });
    afterAnnotationChange(i);
    if (!state.redactWarned) {
      state.redactWarned = true;
      setHint("Black boxes only cover content — they are not certified redaction. Flatten and verify before sharing.");
    }
    return;
  }

  pushHistory();
  entry.annos.push({
    kind,
    rect,
    color: state.color,
    width: state.strokeWidth,
  });
  afterAnnotationChange(i);
}

function afterAnnotationChange(i) {
  drawOverlay(i);
  renderThumb(i);
  renderComments();
  updateStatus();
}

function commitOpenEditor() {
  document.querySelectorAll(".text-editor").forEach((ta) => {
    if (ta.__commit) ta.__commit();
  });
}

function startTextEditor(i, p, mode = "text") {
  const rec = recs[i];
  const entry = state.pages[i];
  if (!rec || !entry) return;
  commitOpenEditor();
  // The shared color control starts on highlight yellow; use a readable dark
  // color for typed text until the user explicitly chooses an annotation color.
  const editorColor = mode === "text" && !state.colorTouched ? "#1f2937" : state.color;

  const ta = document.createElement("textarea");
  ta.className = "text-editor";
  ta.style.left = `${p.x * rec.w}px`;
  ta.style.top = `${p.y * rec.h}px`;
  ta.style.width = `${Math.max(140, Math.min(rec.w * 0.6, 320))}px`;
  ta.style.height = `${mode === "note" ? 72 : Math.max(42, 60)}px`;
  if (mode === "note") ta.placeholder = "Sticky note text…";
  ta.style.fontSize = `${Math.max(9, state.fontSize * (rec.w / (entry.dim?.w || rec.w)))}px`;
  ta.style.color = editorColor;
  rec.surface.appendChild(ta);
  // Focus after the pointer event finishes; otherwise the browser's default
  // focus handling for the click can leave the textarea unfocused, and a
  // later blur()/click never commits the annotation.
  ta.focus();
  setTimeout(() => {
    if (ta.isConnected) ta.focus();
  }, 0);

  let done = false;
  const cancel = () => {
    if (done) return;
    done = true;
    ta.remove();
    drawOverlay(i);
  };
  const commit = () => {
    if (done) return;
    done = true;
    const text = ta.value.trim();
    // Measure while the editor is still in the document.
    const boxW = ta.offsetWidth;
    const boxH = Math.max(ta.scrollHeight, ta.offsetHeight);
    ta.remove();
    if (!text) {
      drawOverlay(i);
      return;
    }
    pushHistory();
    if (mode === "note") {
      const w = Math.max(0.025, 18 / rec.w);
      const h = Math.max(0.02, 18 / rec.h);
      entry.annos.push({
        kind: "note",
        rect: [
          Number(p.x.toFixed(4)),
          Number(p.y.toFixed(4)),
          Number(w.toFixed(4)),
          Number(h.toFixed(4)),
        ],
        text,
        color: state.color,
      });
    } else {
      const rectW = clamp(boxW / rec.w, 0.02, 0.98);
      const rectH = clamp(boxH / rec.h, 0.01, 0.98);
      entry.annos.push({
        kind: "text",
        rect: [
          Number(p.x.toFixed(4)),
          Number(p.y.toFixed(4)),
          Number(rectW.toFixed(4)),
          Number(rectH.toFixed(4)),
        ],
        text,
        color: editorColor,
        size: state.fontSize,
      });
    }
    afterAnnotationChange(i);
  };

  ta.__commit = commit;
  ta.addEventListener("blur", commit);
  ta.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      cancel();
    } else if (ev.key === "Enter" && (ev.ctrlKey || ev.metaKey)) {
      ev.preventDefault();
      commit();
    }
  });
}

function pointInRect(p, rect, pad = 0.004) {
  return (
    p.x >= rect[0] - pad &&
    p.x <= rect[0] + rect[2] + pad &&
    p.y >= rect[1] - pad &&
    p.y <= rect[1] + rect[3] + pad
  );
}

function nearPolyline(p, points, tol = 0.01) {
  for (let i = 1; i < points.length; i++) {
    const [x1, y1] = points[i - 1];
    const [x2, y2] = points[i];
    const dx = x2 - x1;
    const dy = y2 - y1;
    const len2 = dx * dx + dy * dy || 1e-9;
    let t = ((p.x - x1) * dx + (p.y - y1) * dy) / len2;
    t = clamp(t);
    const px = x1 + t * dx;
    const py = y1 + t * dy;
    if (Math.hypot(p.x - px, p.y - py) < tol) return true;
  }
  return false;
}

function eraseAt(i, p) {
  const entry = state.pages[i];
  if (!entry) return;
  for (let k = entry.annos.length - 1; k >= 0; k--) {
    const a = entry.annos[k];
    const hit =
      annotationRects(a).some((rect) => pointInRect(p, rect)) ||
      (a.kind === "ink" && a.points && nearPolyline(p, a.points));
    if (hit) {
      pushHistory();
      entry.annos.splice(k, 1);
      afterAnnotationChange(i);
      return;
    }
  }
}

/* ------------------------------------------------------------------ *
 * text selection highlighting + anchored comments
 * ------------------------------------------------------------------ */

function selectionInTextLayer(sel) {
  const node = sel.anchorNode;
  const el = node?.nodeType === 1 ? node : node?.parentElement;
  return !!el?.closest?.(".textLayer");
}

function mergeSelectionRects(rects) {
  const out = [];
  const sorted = [...rects].sort((a, b) => a[1] - b[1] || a[0] - b[0]);
  for (const rect of sorted) {
    let merged = false;
    for (let i = 0; i < out.length; i++) {
      const cur = out[i];
      const overlap = Math.min(cur[1] + cur[3], rect[1] + rect[3]) - Math.max(cur[1], rect[1]);
      const gap = Math.max(cur[0], rect[0]) - Math.min(cur[0] + cur[2], rect[0] + rect[2]);
      if (overlap > Math.min(cur[3], rect[3]) * 0.5 && gap < 0.006) {
        const x0 = Math.min(cur[0], rect[0]);
        const y0 = Math.min(cur[1], rect[1]);
        const x1 = Math.max(cur[0] + cur[2], rect[0] + rect[2]);
        const y1 = Math.max(cur[1] + cur[3], rect[1] + rect[3]);
        out[i] = [x0, y0, x1 - x0, y1 - y0];
        merged = true;
        break;
      }
    }
    if (!merged) out.push(rect);
  }
  return out;
}

function captureTextSelection() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount || !selectionInTextLayer(sel)) {
    hideSelectionMenu();
    return null;
  }
  const range = sel.getRangeAt(0);
  const text = sel.toString().replace(/\s+/g, " ").trim();
  const clientRects = [...range.getClientRects()].filter((r) => r.width > 1 && r.height > 1);
  if (!text || !clientRects.length) {
    hideSelectionMenu();
    return null;
  }

  const pages = [];
  for (let i = 0; i < recs.length; i++) {
    const rec = recs[i];
    if (!rec) continue;
    const bounds = rec.surface.getBoundingClientRect();
    if (!bounds.width || !bounds.height) continue;
    const rects = [];
    for (const r of clientRects) {
      const left = Math.max(r.left, bounds.left);
      const top = Math.max(r.top, bounds.top);
      const right = Math.min(r.right, bounds.right);
      const bottom = Math.min(r.bottom, bounds.bottom);
      if (right - left < 1 || bottom - top < 1) continue;
      rects.push([
        Number(((left - bounds.left) / bounds.width).toFixed(5)),
        Number(((top - bounds.top) / bounds.height).toFixed(5)),
        Number(((right - left) / bounds.width).toFixed(5)),
        Number(((bottom - top) / bounds.height).toFixed(5)),
      ]);
    }
    if (rects.length) pages.push({ page: i, rects: mergeSelectionRects(rects) });
  }
  if (!pages.length) {
    hideSelectionMenu();
    return null;
  }

  const bounds = range.getBoundingClientRect();
  pendingSelection = { pages, text };
  showSelectionMenu(bounds);
  return pendingSelection;
}

function showSelectionMenu(anchor) {
  if (!els.selectionMenu || !pendingSelection) return;
  els.selectionMenu.classList.remove("hidden");
  requestAnimationFrame(() => {
    if (!pendingSelection || els.selectionMenu.classList.contains("hidden")) return;
    const menu = els.selectionMenu.getBoundingClientRect();
    let left = anchor.left + anchor.width / 2 - menu.width / 2;
    let top = anchor.bottom + 8;
    if (top + menu.height > window.innerHeight - 8) top = anchor.top - menu.height - 8;
    left = Math.max(8, Math.min(left, window.innerWidth - menu.width - 8));
    top = Math.max(8, Math.min(top, window.innerHeight - menu.height - 8));
    els.selectionMenu.style.left = `${Math.round(left)}px`;
    els.selectionMenu.style.top = `${Math.round(top)}px`;
  });
}

function hideSelectionMenu() {
  pendingSelection = null;
  els.selectionMenu?.classList.add("hidden");
}

function highlightSelection(withComment = false) {
  const selection = pendingSelection;
  if (!selection) return;
  const items = [];
  for (const part of selection.pages) {
    const entry = state.pages[part.page];
    if (!entry || !part.rects.length) continue;
    items.push({ page: part.page, entry, rects: part.rects });
  }
  if (!items.length) {
    hideSelectionMenu();
    return;
  }

  pushHistory();
  let first = null;
  for (const item of items) {
    const rects = item.rects.map((r) => r.map((v) => Number(v.toFixed(5))));
    const anno = {
      kind: "highlight",
      rect: unionRects(rects),
      rects,
      quote: selection.text,
      color: state.color,
      width: state.strokeWidth,
    };
    item.entry.annos.push(anno);
    if (!first) first = { page: item.page, anno };
  }
  for (const item of items) {
    drawOverlay(item.page);
    renderThumb(item.page);
  }
  renderComments();
  updateStatus();
  window.getSelection()?.removeAllRanges();
  hideSelectionMenu();
  setHint(withComment ? "Highlight added. Add a comment for it." : "Text highlighted");
  if (withComment && first) {
    requestAnimationFrame(() => openCommentPopover(first.page, first.anno));
  }
}

function annotationAnchor(page, anno) {
  const rec = recs[page];
  const box = unionRects(annotationRects(anno));
  if (!rec || !box) return null;
  const r = rec.surface.getBoundingClientRect();
  return {
    left: r.left + box[0] * r.width,
    top: r.top + box[1] * r.height,
    right: r.left + (box[0] + box[2]) * r.width,
    bottom: r.top + (box[1] + box[3]) * r.height,
  };
}

function positionCommentPopover() {
  if (!commentTarget || els.commentPopover.classList.contains("hidden")) return;
  const anchor = annotationAnchor(commentTarget.page, commentTarget.anno);
  if (!anchor) return;
  const box = els.commentPopover.getBoundingClientRect();
  let left = anchor.right + 10;
  if (left + box.width > window.innerWidth - 8) left = anchor.left - box.width - 10;
  left = Math.max(8, Math.min(left, window.innerWidth - box.width - 8));
  let top = anchor.top;
  if (top + box.height > window.innerHeight - 8) top = window.innerHeight - box.height - 8;
  top = Math.max(8, top);
  els.commentPopover.style.left = `${Math.round(left)}px`;
  els.commentPopover.style.top = `${Math.round(top)}px`;
}

function openCommentPopover(page, anno) {
  if (!anno) return;
  commentTarget = { page, anno };
  els.commentPopoverText.value = anno.text || "";
  els.commentRemove.classList.toggle("hidden", !anno.text);
  els.commentPopover.classList.remove("hidden");
  requestAnimationFrame(() => {
    positionCommentPopover();
    els.commentPopoverText.focus();
    els.commentPopoverText.setSelectionRange(els.commentPopoverText.value.length, els.commentPopoverText.value.length);
  });
}

function closeCommentPopover() {
  commentTarget = null;
  els.commentPopover?.classList.add("hidden");
  if (els.commentPopoverText) els.commentPopoverText.value = "";
}

function commitCommentPopover() {
  if (!commentTarget) return;
  const { page, anno } = commentTarget;
  const entry = state.pages[page];
  if (!entry || !entry.annos.includes(anno)) {
    closeCommentPopover();
    return;
  }
  const next = els.commentPopoverText.value.trim();
  const prev = anno.text || "";
  if (next === prev) {
    closeCommentPopover();
    return;
  }
  pushHistory();
  if (next) anno.text = next;
  else delete anno.text;
  closeCommentPopover();
  afterAnnotationChange(page);
  setHint(next ? "Comment saved" : "Comment removed");
}

function removeComment() {
  if (!commentTarget) return;
  const { page, anno } = commentTarget;
  const entry = state.pages[page];
  if (!entry || !entry.annos.includes(anno) || !anno.text) {
    closeCommentPopover();
    return;
  }
  pushHistory();
  delete anno.text;
  closeCommentPopover();
  afterAnnotationChange(page);
  setHint("Comment removed");
}

/* ------------------------------------------------------------------ *
 * history
 * ------------------------------------------------------------------ */

function snapshot() {
  return JSON.stringify({
    pages: state.pages.map((p) => ({
      docId: p.docId,
      index: p.index,
      rotation: p.rotation,
      intrinsic: p.intrinsic,
      crop: p.crop || null,
      annos: p.annos,
    })),
    bookmarks: [...state.docs].map(([id, doc]) => [id, doc.bookmarks || []]),
  });
}

function pushHistory() {
  state.history.push(snapshot());
  if (state.history.length > 80) state.history.shift();
  state.future.length = 0;
  updateStatus();
}

async function restoreSnapshot(json) {
  const parsed = JSON.parse(json);
  const storedPages = Array.isArray(parsed) ? parsed : parsed.pages;
  const pages = storedPages.map((p) => ({
    ...p,
    searchHits: [],
    dim: null,
  }));
  if (!Array.isArray(parsed) && Array.isArray(parsed.bookmarks)) {
    for (const [docId, bookmarks] of parsed.bookmarks) {
      const doc = state.docs.get(docId);
      if (doc) doc.bookmarks = bookmarks;
    }
  }
  state.pages = pages;
  state.current = Math.min(state.current, pages.length - 1);
  state.search = { query: state.search.query, matches: [], index: -1 };
  await rebuild();
  renderComments();
  renderOutline();
  if (state.search.query) runSearch(state.search.query);
}

async function undo() {
  if (!state.history.length) return;
  state.future.push(snapshot());
  const previous = state.history.pop();
  await restoreSnapshot(previous);
}

async function redo() {
  if (!state.future.length) return;
  state.history.push(snapshot());
  const next = state.future.pop();
  await restoreSnapshot(next);
}

/* ------------------------------------------------------------------ *
 * thumbnails
 * ------------------------------------------------------------------ */

let thumbToken = 0;
const thumbStates = new WeakMap();

async function renderThumbs() {
  const token = ++thumbToken;
  els.thumbs.innerHTML = "";
  for (let i = 0; i < state.pages.length; i++) {
    if (token !== thumbToken) return;
    const item = document.createElement("div");
    item.className = "thumb" + (i === state.current ? " selected" : "");
    const canvas = document.createElement("canvas");
    const num = document.createElement("div");
    num.className = "num";
    num.textContent = String(i + 1);
    item.appendChild(canvas);
    item.appendChild(num);
    item.addEventListener("click", () => selectPageAndReveal(i));
    els.thumbs.appendChild(item);
    renderThumb(i, canvas);
  }
}

async function renderThumb(i, target) {
  if (!state.pages[i]) return;
  const canvas = target || els.thumbs.querySelectorAll(".thumb canvas")[i];
  if (!canvas) return;
  let job = thumbStates.get(canvas);
  if (!job) {
    job = { running: false, pending: false };
    thumbStates.set(canvas, job);
  }
  if (job.running) {
    job.pending = true;
    return;
  }
  job.running = true;
  try {
    do {
      job.pending = false;
      const entry = state.pages[i];
      if (!entry || !canvas.isConnected) break;
      const info = await pageInfo(entry);
      const base = info.page.getViewport({ scale: 1, rotation: info.total });
      const scale = thumbnailWidth() / base.width;
      const vp = info.page.getViewport({ scale, rotation: info.total });
      canvas.width = Math.floor(vp.width);
      canvas.height = Math.floor(vp.height);
      await info.page.render({ canvasContext: canvas.getContext("2d"), viewport: vp }).promise;
      if (entry.annos.length) {
        drawAnnotations(canvas.getContext("2d"), entry, canvas.width, canvas.height);
      }
    } while (job.pending);
  } catch (err) {
    if (err?.name !== "RenderingCancelledException") console.warn("thumb failed", err);
  } finally {
    job.running = false;
  }
}

/* ------------------------------------------------------------------ *
 * page operations
 * ------------------------------------------------------------------ */

async function rebuild(keepPage = true) {
  closeCommentPopover();
  hideSelectionMenu();
  const target = keepPage ? state.current : 0;
  createPageDom();
  await relayout();
  state.current = Math.min(target, Math.max(0, state.pages.length - 1));
  renderThumbs();
  renderOutline();
  renderComments();
  updateStatus();
  if (state.pages.length) scrollToPage(state.current, false);
}

function selectPage(i) {
  state.current = i;
  els.thumbs.querySelectorAll(".thumb").forEach((el, idx) => {
    el.classList.toggle("selected", idx === i);
  });
  state.pages.forEach((_, idx) => drawOverlay(idx));
  updateStatus();
}

function scrollToPage(i, smooth = true) {
  const rec = recs[i];
  if (!rec) return;
  rec.wrap.scrollIntoView({ block: "start", behavior: smooth ? "smooth" : "auto" });
}

// Select a page from the thumbnail strip/bookmarks/comments and bring it into
// view. Painting is kicked off explicitly so the selected page is ready even
// if the intersection observer has not fired yet.
function selectPageAndReveal(i) {
  if (i < 0 || i >= state.pages.length) return;
  selectPage(i);
  const rec = recs[i];
  if (rec) {
    visible.add(i);
    paintIfStale(i);
  }
  scrollToPage(i, true);
}

async function rotatePage(dir) {
  const index = state.current;
  const entry = state.pages[index];
  if (!entry) return;
  pushHistory();
  entry.rotation = (((entry.rotation + dir * 90) % 360) + 360) % 360;
  invalidate(entry);
  await relayout();
  // Keep the rotated page's thumbnail in sync, even if the selection moves.
  await renderThumb(index);
}

async function deletePage() {
  if (state.pages.length <= 1) {
    setHint("Cannot delete the only page");
    return;
  }
  pushHistory();
  state.pages.splice(state.current, 1);
  await rebuild();
}

async function movePage(dir) {
  const from = state.current;
  const to = from + dir;
  if (to < 0 || to >= state.pages.length) return;
  pushHistory();
  const pages = state.pages;
  [pages[from], pages[to]] = [pages[to], pages[from]];
  state.current = to;
  await rebuild();
}

async function reversePages() {
  if (state.pages.length < 2) return;
  pushHistory();
  const was = state.current;
  state.pages.reverse();
  state.current = state.pages.length - 1 - was;
  await rebuild();
}

async function insertBlankPage() {
  const firstPage = state.pages.length === 0;
  const entry = firstPage ? null : state.pages[state.current];
  const width = entry?.dim?.w || 612;
  const height = entry?.dim?.h || 792;
  try {
    setHint(firstPage ? "Creating a blank PDF…" : "Inserting a blank page…");
    const base64 = await invoke("blank_pdf", { width, height });
    const pdf = await pdfjsLib.getDocument({ data: base64ToBytes(base64) }).promise;
    const id = uid();
    state.docs.set(id, {
      base64,
      pdf,
      name: firstPage ? "Blank document" : "Blank page",
      outline: null,
      bookmarks: [],
    });
    const blank = {
      docId: id,
      index: 0,
      rotation: 0,
      intrinsic: null,
      annos: [],
      crop: null,
      searchHits: [],
      dim: null,
    };
    pushHistory();
    if (firstPage) {
      state.pages = [blank];
      state.current = 0;
      state.path = null;
      state.search = { query: "", matches: [], index: -1 };
      els.search.value = "";
      els.searchLabel.textContent = "";
    } else {
      state.pages.splice(state.current + 1, 0, blank);
      state.current += 1;
    }
    await rebuild();
    setHint(firstPage ? "Created a blank PDF" : "Inserted a blank page");
  } catch (err) {
    alert(`Could not create a blank page:\n${err}`);
  }
}

async function clearCrop() {
  const entry = state.pages[state.current];
  if (!entry?.crop) return;
  pushHistory();
  entry.crop = null;
  drawOverlay(state.current);
  updateStatus();
  setHint("Crop removed");
}

/* ------------------------------------------------------------------ *
 * compose / save
 * ------------------------------------------------------------------ */

function securityPayload() {
  const security = state.security;
  if (!security?.enabled) return null;
  return {
    user_password: security.userPassword,
    owner_password: security.ownerPassword || "",
    allow_print: !!security.allowPrint,
    allow_copy: !!security.allowCopy,
    allow_modify: !!security.allowModify,
    allow_annotate: !!security.allowAnnotate,
    allow_assemble: !!security.allowAssemble,
  };
}

function protectionReady() {
  if (!state.security.enabled) return true;
  if (!state.security.userPassword) {
    alert("Set an open password in Options before saving a protected PDF.");
    return false;
  }
  return true;
}

async function buildPayload(pages, extras = true, includeSecurity = true) {
  for (const entry of pages) await pageInfo(entry);
  const usedDocs = new Set(pages.map((entry) => entry.docId));
  const docs = [...state.docs.entries()]
    .filter(([id]) => usedDocs.has(id))
    .map(([id, doc]) => ({ id, data: doc.base64 }));
  const pageSpecs = pages.map((entry) => {
    const spec = {
      doc: entry.docId,
      index: entry.index,
      rotation: ((((entry.intrinsic || 0) + entry.rotation) % 360) + 360) % 360,
    };
    if (entry.crop) spec.crop = entry.crop;
    return spec;
  });
  const annotations = [];
  pages.forEach((entry, index) => {
    for (const a of entry.annos) {
      const item = {
        page: index,
        kind: a.kind,
        color: hexToRgb01(a.color || "#000000").map((v) => Number(v.toFixed(4))),
        opacity: a.kind === "highlight" ? 0.35 : 1,
        width: a.width || 2,
        size: a.size || 12,
      };
      if (a.rect) item.rect = a.rect;
      if (a.rects) item.rects = a.rects;
      if (a.points) item.points = a.points;
      if (a.text) item.text = a.text;
      if (a.label) item.label = a.label;
      if (a.image) item.image = a.image;
      annotations.push(item);
    }
  });

  const s = state.settings;
  const payload = {
    docs,
    pages: pageSpecs,
    annotations,
    flatten: !!s.flatten,
    remove_metadata: !!s.removeMetadata,
  };
  const bookmarks = [];
  for (const [docId, doc] of state.docs) {
    for (const bookmark of doc.bookmarks || []) {
      const page = pages.findIndex((entry) => entry.docId === docId && entry.index === bookmark.page);
      if (page >= 0) bookmarks.push({ title: bookmark.title, page });
    }
  }
  if (bookmarks.length) payload.bookmarks = bookmarks;
  if (extras) {
    payload.page_numbers = !!s.pageNumbers && pages.length === state.pages.length;
    if (s.watermark.trim()) {
      payload.watermark = {
        text: s.watermark.trim(),
        size: 54,
        opacity: Number(s.watermarkOpacity) || 0.12,
        angle: Number(s.watermarkAngle) || 45,
        color: [0.45, 0.45, 0.45],
      };
    }
    if (!s.removeMetadata) {
      const info = {};
      for (const key of ["title", "author", "subject", "keywords"]) {
        const value = String(s.info[key] || "").trim();
        if (value) info[key] = value;
      }
      if (Object.keys(info).length) payload.info = info;
    }
  }
  const security = includeSecurity ? securityPayload() : null;
  if (security) payload.security = security;
  return JSON.stringify(payload);
}

async function saveDocument(asNew) {
  if (!state.pages.length) return;
  if (!protectionReady()) return;
  let path = state.path;
  if (asNew || !path) {
    path = await saveDialog({
      defaultPath: path ? path.replace(/\.pdf$/i, " (edited).pdf") : "document.pdf",
      filters: [{ name: "PDF", extensions: ["pdf"] }],
    });
    if (!path) return;
  }
  setHint("Saving…");
  try {
    const payload = await buildPayload(state.pages);
    const base64 = await invoke("build_pdf", { payload });
    await invoke("save_pdf", { path, data: base64 });
    state.path = path;
    setHint(`Saved${state.security.enabled ? " (AES-256 protected)" : ""} ${path}`);
  } catch (err) {
    setHint("Save failed");
    alert(`Could not save PDF:\n${err}`);
  }
}

async function extractPage() {
  const entry = state.pages[state.current];
  if (!entry) return;
  if (!protectionReady()) return;
  const path = await saveDialog({
    defaultPath: `${docBaseName()}-page-${state.current + 1}.pdf`,
    filters: [{ name: "PDF", extensions: ["pdf"] }],
  });
  if (!path) return;
  try {
    const payload = await buildPayload([entry], false);
    const base64 = await invoke("build_pdf", { payload });
    await invoke("save_pdf", { path, data: base64 });
    setHint(`Saved page to ${path}`);
  } catch (err) {
    alert(`Could not extract page:\n${err}`);
  }
}

async function splitDocument() {
  if (!state.pages.length) return;
  if (!protectionReady()) return;
  const dir = await openDialog({ directory: true, multiple: false, title: "Choose a folder for split pages" });
  if (!dir) return;
  setHint("Splitting…");
  try {
    const base = docBaseName();
    for (let i = 0; i < state.pages.length; i++) {
      const payload = await buildPayload([state.pages[i]], false);
      const data = await invoke("build_pdf", { payload });
      await invoke("save_pdf", {
        path: joinPath(dir, `${base}-page-${String(i + 1).padStart(3, "0")}.pdf`),
        data,
      });
    }
    setHint(`Split ${state.pages.length} pages into ${dir}`);
  } catch (err) {
    alert(`Could not split document:\n${err}`);
  }
}

/* ------------------------------------------------------------------ *
 * open / add pages
 * ------------------------------------------------------------------ */

async function outlineDestPage(pdf, dest) {
  try {
    let target = dest;
    if (typeof dest === "string") target = await pdf.getDestination(dest);
    if (!Array.isArray(target) || !target.length) return null;
    const ref = target[0];
    if (ref && typeof ref === "object") return await pdf.getPageIndex(ref);
    if (typeof ref === "number") return ref - 1;
  } catch {
    /* broken destination */
  }
  return null;
}

async function flattenOutline(pdf, items, depth = 0) {
  const result = [];
  for (const item of items || []) {
    if (!item) continue;
    const page = await outlineDestPage(pdf, item.dest);
    if (Number.isInteger(page) && page >= 0) {
      result.push({
        id: uid(),
        title: item.title || "Bookmark",
        page,
        depth,
      });
    }
    if (Array.isArray(item.items) && item.items.length) {
      result.push(...(await flattenOutline(pdf, item.items, depth + 1)));
    }
  }
  return result;
}

function pdfColorHex(color, fallback = "#ffd400") {
  let values = Array.from(color || []).slice(0, 3).map(Number).filter(Number.isFinite);
  if (values.length < 3) return fallback;
  if (values.every((value) => value <= 1)) values = values.map((value) => value * 255);
  return `#${values
    .map((value) => Math.round(clamp(value, 0, 255)).toString(16).padStart(2, "0"))
    .join("")}`;
}

function pdfPointNormalized(viewport, x, y) {
  const point = viewport.convertToViewportPoint(x, y);
  return [clamp(point[0] / viewport.width), clamp(point[1] / viewport.height)];
}

function pdfRectNormalized(viewport, rect) {
  if (!rect || rect.length < 4) return null;
  const points = [
    pdfPointNormalized(viewport, rect[0], rect[1]),
    pdfPointNormalized(viewport, rect[2], rect[1]),
    pdfPointNormalized(viewport, rect[0], rect[3]),
    pdfPointNormalized(viewport, rect[2], rect[3]),
  ];
  const x0 = Math.min(...points.map((point) => point[0]));
  const y0 = Math.min(...points.map((point) => point[1]));
  const x1 = Math.max(...points.map((point) => point[0]));
  const y1 = Math.max(...points.map((point) => point[1]));
  return [x0, y0, Math.max(0.001, x1 - x0), Math.max(0.001, y1 - y0)];
}

function pdfQuadsNormalized(viewport, quads) {
  if (!quads || quads.length < 8) return [];
  const out = [];
  for (let i = 0; i + 7 < quads.length; i += 8) {
    const points = [
      pdfPointNormalized(viewport, quads[i], quads[i + 1]),
      pdfPointNormalized(viewport, quads[i + 2], quads[i + 3]),
      pdfPointNormalized(viewport, quads[i + 4], quads[i + 5]),
      pdfPointNormalized(viewport, quads[i + 6], quads[i + 7]),
    ];
    const x0 = Math.min(...points.map((point) => point[0]));
    const y0 = Math.min(...points.map((point) => point[1]));
    const x1 = Math.max(...points.map((point) => point[0]));
    const y1 = Math.max(...points.map((point) => point[1]));
    if (x1 - x0 > 0.001 && y1 - y0 > 0.001) out.push([x0, y0, x1 - x0, y1 - y0]);
  }
  return out;
}

async function annotationsFromPdfPage(page) {
  let source;
  try {
    source = await page.getAnnotations({ intent: "display" });
  } catch {
    return [];
  }
  const viewport = page.getViewport({ scale: 1, rotation: page.rotate || 0 });
  const out = [];
  for (const annotation of source || []) {
    const subtype = String(annotation.subtype || "").toLowerCase();
    if (subtype === "popup" || subtype === "link" || subtype === "widget") continue;
    const rect = pdfRectNormalized(viewport, annotation.rect);
    if (!rect) continue;
    const contents = String(annotation.contentsObj?.str ?? annotation.contents ?? "").trim();
    const color = pdfColorHex(annotation.color);
    if (subtype === "highlight" || subtype === "underline" || subtype === "strikeout" || subtype === "squiggly") {
      const quads = pdfQuadsNormalized(viewport, annotation.quadPoints);
      const rects = quads.length ? quads : [rect];
      out.push({
        kind: subtype,
        rect: unionRects(rects),
        rects,
        color,
        text: contents || undefined,
      });
    } else if (subtype === "text") {
      out.push({ kind: "note", rect, color, text: contents || undefined });
    } else if (subtype === "freetext") {
      out.push({
        kind: "text",
        rect,
        color: pdfColorHex(annotation.color, "#1f2937"),
        text: contents || undefined,
        size: Number(annotation.fontSize) || 12,
      });
    } else if (subtype === "square") {
      out.push({ kind: "square", rect, color, width: Number(annotation.borderStyle?.width) || 2 });
    }
  }
  return out;
}

async function openBase64Pdf(base64, name, mode = "replace", path = null) {
  const pdf = await pdfjsLib.getDocument({ data: base64ToBytes(base64) }).promise;
  const id = uid();
  let outline = null;
  try {
    outline = await pdf.getOutline();
  } catch {
    outline = null;
  }
  const bookmarks = await flattenOutline(pdf, outline);
  const docObj = { base64, pdf, name, outline, bookmarks };
  const entries = [];
  for (let i = 0; i < pdf.numPages; i++) {
    const page = await pdf.getPage(i + 1);
    entries.push({
      docId: id,
      index: i,
      rotation: 0,
      intrinsic: page.rotate || 0,
      annos: await annotationsFromPdfPage(page),
      crop: null,
      searchHits: [],
      dim: null,
    });
  }

  if (mode === "replace") {
    state.docs = new Map([[id, docObj]]);
    state.pages = entries;
    state.current = 0;
    state.path = path;
    state.history = [];
    state.future = [];
    state.search = { query: "", matches: [], index: -1 };
    els.search.value = "";
    els.searchLabel.textContent = "";
  } else {
    state.docs.set(id, docObj);
    pushHistory();
    const at = state.pages.length ? state.current + 1 : 0;
    state.pages.splice(at, 0, ...entries);
    state.current = at;
  }

  els.empty.style.display = "none";
  await rebuild();
  setHint(`${state.pages.length} page${state.pages.length === 1 ? "" : "s"} loaded`);
}

let passwordResolve = null;

function askPassword(path, wrong = false) {
  return new Promise((resolve) => {
    passwordResolve = resolve;
    const name = path.split(/[\\/]/).pop() || path;
    els.passwordMessage.textContent = wrong
      ? `That password was not accepted for “${name}”. Try again.`
      : `“${name}” is password protected. Enter its password.`;
    els.passwordError.textContent = "";
    els.passwordInput.value = "";
    els.modalPassword.classList.remove("hidden");
    setTimeout(() => els.passwordInput.focus(), 0);
  });
}

function closePassword(value) {
  if (!passwordResolve) return;
  const resolve = passwordResolve;
  passwordResolve = null;
  els.modalPassword.classList.add("hidden");
  resolve(value);
}

function submitPassword() {
  const value = els.passwordInput.value;
  if (!value) {
    els.passwordError.textContent = "Enter the password.";
    return;
  }
  closePassword(value);
}

async function openPath(path, mode = "replace", password = null) {
  setHint("Opening…");
  try {
    const result = await invoke("open_pdf", { path, password });
    if (result.encrypted) {
      if (mode === "replace" || !state.security.enabled) {
        state.security = {
          ...defaultSecurity(),
          enabled: true,
          userPassword: password || "",
          ownerPassword: password || "",
        };
      }
    } else if (mode === "replace") {
      state.security = defaultSecurity();
    }
    await openBase64Pdf(result.data, path, mode, path);
    if (result.seal === "invalid") {
      alert(
        "Integrity warning: this PDF's PDFzen seal does not match. The file may have been changed since it was saved.",
      );
      setHint("Opened with an integrity warning");
    } else if (result.seal === "valid") {
      setHint("Opened — integrity verified");
    } else if (result.encrypted && !state.security.userPassword) {
      setHint("Opened — no open password; set one in Options before saving");
    }
  } catch (err) {
    const text = String(err);
    if (text.includes("PASSWORD_REQUIRED") || text.includes("WRONG_PASSWORD")) {
      const pwd = await askPassword(path, text.includes("WRONG_PASSWORD"));
      if (pwd == null) {
        setHint("Open cancelled");
        return;
      }
      return openPath(path, mode, pwd);
    }
    setHint("Open failed");
    alert(`Could not open PDF:\n${err}`);
  }
}

async function chooseOpen() {
  const path = await openDialog({
    multiple: false,
    filters: [{ name: "PDF", extensions: ["pdf"] }],
  });
  if (!path) return;
  await openPath(path, "replace");
}

async function chooseAdd() {
  const paths = await openDialog({
    multiple: true,
    filters: [{ name: "PDF", extensions: ["pdf"] }],
  });
  if (!paths) return;
  const list = Array.isArray(paths) ? paths : [paths];
  for (const p of list) await openPath(p, state.pages.length ? "insert" : "replace");
}

/* ------------------------------------------------------------------ *
 * outline & comments
 * ------------------------------------------------------------------ */

function pageForBookmark(docId, index) {
  return state.pages.findIndex((entry) => entry.docId === docId && entry.index === index);
}

function renderOutline() {
  els.outline.innerHTML = "";
  let found = false;
  for (const [docId, doc] of state.docs) {
    const bookmarks = doc.bookmarks || [];
    if (!bookmarks.length) continue;
    found = true;
    if (state.docs.size > 1) {
      const head = document.createElement("div");
      head.className = "comments-empty";
      head.textContent = (doc.name || "Document").split(/[\\/]/).pop();
      els.outline.appendChild(head);
    }
    bookmarks.forEach((bookmark, index) => {
      const page = pageForBookmark(docId, bookmark.page);
      const row = document.createElement("div");
      row.className = "outline-row";
      const btn = document.createElement("button");
      btn.className = "outline-item";
      btn.title = bookmark.title;
      btn.style.paddingLeft = `${6 + (bookmark.depth || 0) * 12}px`;
      btn.textContent = bookmark.title;
      if (page >= 0) {
        const pageLabel = document.createElement("span");
        pageLabel.className = "bookmark-page";
        pageLabel.textContent = String(page + 1);
        btn.appendChild(pageLabel);
      }
      btn.addEventListener("click", () => {
        if (page >= 0) selectPageAndReveal(page);
      });
      const del = document.createElement("button");
      del.className = "outline-del";
      del.title = "Delete bookmark";
      del.textContent = "×";
      del.addEventListener("click", (ev) => {
        ev.stopPropagation();
        pushHistory();
        doc.bookmarks.splice(index, 1);
        renderOutline();
        setHint("Bookmark deleted");
      });
      row.appendChild(btn);
      row.appendChild(del);
      els.outline.appendChild(row);
    });
  }
  if (!found) {
    const empty = document.createElement("div");
    empty.className = "outline-empty";
    empty.textContent = "No bookmarks yet. Add one for the current page.";
    els.outline.appendChild(empty);
  }
}

function addBookmark() {
  const entry = state.pages[state.current];
  if (!entry) return;
  const doc = state.docs.get(entry.docId);
  if (!doc) return;
  const title = els.bookmarkTitle.value.trim() || `Page ${state.current + 1}`;
  pushHistory();
  if (!Array.isArray(doc.bookmarks)) doc.bookmarks = [];
  doc.bookmarks.push({
    id: uid(),
    title,
    page: entry.index,
    depth: 0,
  });
  els.bookmarkTitle.value = "";
  renderOutline();
  setHint("Bookmark added");
}

function addComment() {
  const entry = state.pages[state.current];
  if (!entry) return;
  const text = els.commentDraft.value.trim();
  if (!text) {
    setHint("Type a comment first");
    els.commentDraft.focus();
    return;
  }
  pushHistory();
  entry.annos.push({
    kind: "note",
    rect: [0.88, 0.05, 0.025, 0.02],
    text,
    color: state.color,
  });
  els.commentDraft.value = "";
  afterAnnotationChange(state.current);
  renderComments();
  setHint("Comment added to this page");
}

function renderComments() {
  if (!els.comments) return;
  els.comments.innerHTML = "";
  let total = 0;
  state.pages.forEach((entry, page) => {
    entry.annos.forEach((a, index) => {
      if (a.kind === "redact") {
        // Black boxes are still listed so they can be found and removed.
      }
      total++;
      const item = document.createElement("div");
      item.className = "comment-item";
      const head = document.createElement("div");
      head.className = "comment-head";
      const swatch = document.createElement("span");
      swatch.className = "swatch";
      swatch.style.background = a.color || "#000";
      const label = document.createElement("span");
      label.textContent = `${page + 1} · ${KIND_LABELS[a.kind] || a.kind}`;
      const edit = document.createElement("button");
      edit.className = "comment-edit";
      edit.title = a.text ? "Edit comment" : "Add a comment";
      edit.textContent = a.text ? "Edit" : "Comment";
      edit.addEventListener("click", (ev) => {
        ev.stopPropagation();
        if (state.current !== page) {
          selectPage(page);
          scrollToPage(page, false);
        }
        openCommentPopover(page, a);
      });
      const del = document.createElement("button");
      del.className = "comment-del";
      del.title = "Delete annotation";
      del.textContent = "✕";
      del.addEventListener("click", (ev) => {
        ev.stopPropagation();
        pushHistory();
        entry.annos.splice(index, 1);
        afterAnnotationChange(page);
      });
      head.appendChild(swatch);
      head.appendChild(label);
      if (MARKUP_KINDS.has(a.kind)) head.appendChild(edit);
      head.appendChild(del);

      const body = document.createElement("div");
      body.className = "comment-body";
      if (a.text) {
        body.textContent = a.text;
      } else if (a.quote) {
        body.textContent = `“${a.quote}”`;
        body.classList.add("comment-quote");
      } else {
        body.textContent = a.label || (a.kind === "image" ? "Signature" : "");
        if (!body.textContent) body.textContent = KIND_LABELS[a.kind] || a.kind;
      }

      item.title = a.quote || "";
      item.appendChild(head);
      item.appendChild(body);
      item.addEventListener("click", () => {
        selectPageAndReveal(page);
      });
      els.comments.appendChild(item);
    });
  });

  if (!total) {
    const empty = document.createElement("div");
    empty.className = "comments-empty";
    empty.textContent = "No comments yet. Select text to highlight it, or add a note.";
    els.comments.appendChild(empty);
  }
}

/* ------------------------------------------------------------------ *
 * search
 * ------------------------------------------------------------------ */

// Search uses the rendered text layer when possible so the highlight covers
// only the matched characters. The text-item fallback keeps search working for
// pages whose text layer could not be rendered.
function searchTextLayerMatches(page, q) {
  const rec = recs[page];
  if (!rec || !rec.textLayer) return [];
  const bounds = rec.surface.getBoundingClientRect();
  if (!bounds.width || !bounds.height) return [];

  const matches = [];
  for (const span of rec.textLayer.querySelectorAll("span")) {
    const walker = document.createTreeWalker(span, NodeFilter.SHOW_TEXT);
    let node;
    while ((node = walker.nextNode())) {
      const text = node.data || "";
      const lower = text.toLowerCase();
      let at = lower.indexOf(q);
      while (at >= 0) {
        const range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + q.length);
        const rects = [];
        for (const r of range.getClientRects()) {
          const left = Math.max(r.left, bounds.left);
          const top = Math.max(r.top, bounds.top);
          const right = Math.min(r.right, bounds.right);
          const bottom = Math.min(r.bottom, bounds.bottom);
          if (right - left < 0.4 || bottom - top < 0.4) continue;
          rects.push([
            (left - bounds.left) / bounds.width,
            (top - bounds.top) / bounds.height,
            (right - left) / bounds.width,
            (bottom - top) / bounds.height,
          ]);
        }
        if (rects.length) matches.push({ page, rects: mergeSelectionRects(rects) });
        at = lower.indexOf(q, at + q.length);
      }
    }
  }
  return matches;
}

function searchTextItemMatches(page, content, q, vp) {
  const matches = [];
  for (const item of content.items) {
    if (!item.str) continue;
    const text = item.str;
    const lower = text.toLowerCase();
    let at = lower.indexOf(q);
    while (at >= 0) {
      const end = at + q.length;
      const [a, b, c, d, e, f] = item.transform;
      const itemWidth = item.width || 0;
      const h = item.height || Math.abs(d) || 10;
      const p1 = vp.convertToViewportPoint(e + (itemWidth * at) / text.length, f);
      const p2 = vp.convertToViewportPoint(e + (itemWidth * end) / text.length, f + h);
      const x = Math.min(p1[0], p2[0]) / vp.width;
      const y = Math.min(p1[1], p2[1]) / vp.height;
      const w = Math.abs(p1[0] - p2[0]) / vp.width;
      const hh = Math.abs(p1[1] - p2[1]) / vp.height;
      matches.push({
        page,
        rects: [[x, y, Math.max(w, 0.002), Math.max(hh, 0.006)]],
      });
      at = lower.indexOf(q, end);
    }
  }
  return matches;
}

let searchRequest = 0;

async function runSearch(query) {
  const request = ++searchRequest;
  const q = query.trim().toLowerCase();
  state.search = { query: q, matches: [], index: -1 };
  state.pages.forEach((entry) => (entry.searchHits = []));
  if (!q) {
    els.searchLabel.textContent = "";
    state.pages.forEach((_, i) => drawOverlay(i));
    return;
  }

  setHint("Searching…");
  outer: for (let i = 0; i < state.pages.length; i++) {
    const entry = state.pages[i];
    try {
      const info = await pageInfo(entry);
      const content = await info.page.getTextContent();
      if (request !== searchRequest) return;
      if (!content.items.some((item) => item.str && item.str.toLowerCase().includes(q))) {
        continue;
      }

      await paintTextLayer(i);
      if (request !== searchRequest) return;
      const rec = recs[i];
      const textLayerReady =
        rec && rec.textScale === state.scale && rec.textTotal === entry.dim?.total;
      let pageMatches = textLayerReady ? searchTextLayerMatches(i, q) : [];
      if (!pageMatches.length) {
        const vp = info.page.getViewport({ scale: 1, rotation: info.total });
        pageMatches = searchTextItemMatches(i, content, q, vp);
      }
      for (const match of pageMatches) {
        state.search.matches.push(match);
        if (state.search.matches.length >= 500) break outer;
      }
    } catch (err) {
      console.warn("search page failed", err);
    }
  }
  if (request !== searchRequest) return;

  state.search.index = state.search.matches.length ? 0 : -1;
  els.searchLabel.textContent = state.search.matches.length
    ? `1/${state.search.matches.length}`
    : "none";
  state.pages.forEach((_, i) => drawOverlay(i));
  if (state.search.index >= 0) gotoMatch(0);
  setHint(
    state.search.matches.length
      ? `${state.search.matches.length} match(es) found`
      : "No matches found",
  );
}

function gotoMatch(index) {
  const matches = state.search.matches;
  if (!matches.length) return;
  const i = ((index % matches.length) + matches.length) % matches.length;
  state.search.index = i;
  const match = matches[i];
  els.searchLabel.textContent = `${i + 1}/${matches.length}`;
  if (state.current !== match.page) {
    selectPageAndReveal(match.page);
  }
  state.pages.forEach((_, idx) => drawOverlay(idx));
}

/* ------------------------------------------------------------------ *
 * zoom
 * ------------------------------------------------------------------ */

async function setScale(next) {
  const clamped = clamp(next, 0.2, 6);
  if (Math.abs(clamped - state.scale) < 0.0001) return;
  state.scale = clamped;
  await relayout();
}

async function fitWidth() {
  const entry = state.pages[state.current];
  if (!entry) return;
  await pageInfo(entry);
  if (!entry.dim) {
    const info = await pageInfo(entry);
    const vp = info.page.getViewport({ scale: 1, rotation: info.total });
    entry.dim = { w: vp.width, h: vp.height, total: info.total };
  }
  const avail = els.viewer.clientWidth - 48;
  state.scale = clamp(avail / entry.dim.w, 0.2, 6);
  await relayout();
}

/* ------------------------------------------------------------------ *
 * export
 * ------------------------------------------------------------------ */

async function renderPageCanvas(entry, scale) {
  const info = await pageInfo(entry);
  const vp = info.page.getViewport({ scale, rotation: info.total });
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.floor(vp.width));
  canvas.height = Math.max(1, Math.floor(vp.height));
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  await info.page.render({ canvasContext: ctx, viewport: vp }).promise;
  return canvas;
}

async function saveBytes(path, bytes) {
  await invoke("save_pdf", { path, data: bytesToBase64(bytes) });
}

async function exportText() {
  if (!state.pages.length) return;
  const path = await saveDialog({
    defaultPath: `${docBaseName()}.txt`,
    filters: [{ name: "Text", extensions: ["txt"] }],
  });
  if (!path) return;
  setHint("Extracting text…");
  const chunks = [];
  for (let i = 0; i < state.pages.length; i++) {
    try {
      const info = await pageInfo(state.pages[i]);
      const content = await info.page.getTextContent();
      let buf = "";
      for (const item of content.items) {
        if (!item.str) continue;
        buf += item.str;
        buf += item.hasEOL ? "\n" : " ";
      }
      chunks.push(`--- Page ${i + 1} ---\n${buf.trim()}`);
    } catch (err) {
      console.warn("text export failed", err);
    }
  }
  const text = chunks.join("\n\n");
  await saveBytes(path, new TextEncoder().encode(text));
  setHint(`Exported text to ${path}`);
}

async function exportPng() {
  const entry = state.pages[state.current];
  if (!entry) return;
  const path = await saveDialog({
    defaultPath: `${docBaseName()}-page-${state.current + 1}.png`,
    filters: [{ name: "PNG image", extensions: ["png"] }],
  });
  if (!path) return;
  setHint("Rendering page…");
  try {
    const canvas = await renderPageCanvas(entry, 2);
    const data = canvas.toDataURL("image/png").split(",")[1];
    await invoke("save_pdf", { path, data });
    setHint(`Saved ${path}`);
  } catch (err) {
    alert(`Could not export PNG:\n${err}`);
  }
}

async function exportEmbeddedImages() {
  if (!state.pages.length) return;
  const dir = await openDialog({ directory: true, multiple: false, title: "Choose a folder for extracted images" });
  if (!dir) return;
  setHint("Extracting images…");
  try {
    const payload = await buildPayload(state.pages, false, false);
    const base64 = await invoke("build_pdf", { payload });
    const images = await invoke("extract_pdf_images", { data: base64 });
    if (!images || !images.length) {
      setHint("No embedded raster images found (vector art and JPEG-only pages may be skipped).");
      return;
    }
    let n = 0;
    for (const img of images) {
      const name = `page-${String(img.page).padStart(3, "0")}-${sanitizeName(img.name)}.${img.ext}`;
      await invoke("save_pdf", { path: joinPath(dir, name), data: img.data });
      n++;
    }
    setHint(`Extracted ${n} image${n === 1 ? "" : "s"} to ${dir}`);
  } catch (err) {
    alert(`Could not extract images:\n${err}`);
  }
}

async function createPdfFromImages() {
  const paths = await openDialog({
    multiple: true,
    title: "Choose images",
    filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg"] }],
  });
  if (!paths) return;
  const list = Array.isArray(paths) ? paths : [paths];
  if (!list.length) return;
  setHint("Building PDF from images…");
  try {
    const base64 = await invoke("create_pdf_from_images", { paths: list });
    await openBase64Pdf(base64, "Images", state.pages.length ? "insert" : "replace", state.path);
    setHint(`Added ${list.length} image page${list.length === 1 ? "" : "s"}`);
  } catch (err) {
    alert(`Could not create PDF from images:\n${err}`);
  }
}

async function printDocument() {
  if (!state.pages.length) return;
  setHint("Preparing print…");
  els.printArea.innerHTML = "";
  try {
    for (const entry of state.pages) {
      const canvas = await renderPageCanvas(entry, 2);
      els.printArea.appendChild(canvas);
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
    window.print();
    setHint("Print dialog opened");
  } catch (err) {
    console.warn("print failed", err);
    alert(`Could not print:\n${err}`);
  }
}

/* ------------------------------------------------------------------ *
 * signature pad
 * ------------------------------------------------------------------ */

let signDrawing = false;
let signHasInk = false;
let signLast = null;
let signMode = "type";

const SIGN_FONTS = {
  segoe: { family: '"Segoe Script", "Brush Script MT", cursive', italic: false },
  brush: { family: '"Brush Script MT", "Segoe Script", cursive', italic: false },
  handwriting: { family: '"Lucida Handwriting", "Segoe Script", cursive', italic: false },
  formal: { family: 'Georgia, "Times New Roman", serif', italic: true },
};

function signatureFont(size) {
  const font = SIGN_FONTS[els.signFont.value] || SIGN_FONTS.segoe;
  return `${font.italic ? "italic " : ""}${size}px ${font.family}`;
}

function clearSignPad() {
  const canvas = els.signPad;
  const ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.strokeStyle = "#111";
  ctx.lineWidth = 3;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  signHasInk = false;
  signLast = null;
}

function drawTypedSignature() {
  const canvas = els.signPreview;
  const ctx = canvas.getContext("2d");
  const name = els.signName.value.trim();
  ctx.clearRect(0, 0, canvas.width, canvas.height);

  let size = 86;
  ctx.font = signatureFont(size);
  const nameWidth = ctx.measureText(name || "Your name").width;
  const maxWidth = canvas.width * 0.84;
  if (nameWidth > maxWidth) size = Math.max(26, Math.floor((size * maxWidth) / nameWidth));
  ctx.font = signatureFont(size);
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillStyle = name ? els.signColor.value : "rgba(20, 33, 61, 0.25)";
  ctx.fillText(name || "Your name", canvas.width / 2, canvas.height * 0.54);
}

function signPos(ev) {
  const canvas = els.signPad;
  const r = canvas.getBoundingClientRect();
  return {
    x: ((ev.clientX - r.left) / r.width) * canvas.width,
    y: ((ev.clientY - r.top) / r.height) * canvas.height,
  };
}

function setSignMode(mode) {
  signMode = mode;
  document.querySelectorAll(".sign-tab").forEach((tab) => {
    const active = tab.dataset.signMode === mode;
    tab.classList.toggle("active", active);
    tab.setAttribute("aria-selected", String(active));
  });
  document.getElementById("sign-type-pane").classList.toggle("hidden", mode !== "type");
  document.getElementById("sign-draw-pane").classList.toggle("hidden", mode !== "draw");
  if (mode === "type") {
    drawTypedSignature();
    els.signName.focus();
  }
}

function clearSignatureInput() {
  if (signMode === "type") {
    els.signName.value = "";
    drawTypedSignature();
    els.signName.focus();
  } else {
    clearSignPad();
  }
}

function openSignModal() {
  els.signName.value = "";
  clearSignPad();
  drawTypedSignature();
  setSignMode("type");
  els.modalSign.classList.remove("hidden");
}

function closeSignModal() {
  els.modalSign.classList.add("hidden");
}

function useSignature() {
  let data;
  if (signMode === "type") {
    if (!els.signName.value.trim()) {
      setHint("Type your name to create a signature");
      els.signName.focus();
      return;
    }
    drawTypedSignature();
    data = els.signPreview.toDataURL("image/png").split(",")[1];
  } else {
    if (!signHasInk) {
      setHint("Draw a signature first");
      return;
    }
    data = els.signPad.toDataURL("image/png").split(",")[1];
  }
  state.pendingImage = { data };
  closeSignModal();
  setTool("image");
  setHint("Drag a rectangle on the page to place your signature");
}

/* ------------------------------------------------------------------ *
 * options
 * ------------------------------------------------------------------ */

function loadSettings() {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY) || localStorage.getItem(LEGACY_SETTINGS_KEY);
    if (!raw) return;
    if (!localStorage.getItem(SETTINGS_KEY)) {
      localStorage.setItem(SETTINGS_KEY, raw);
      localStorage.removeItem(LEGACY_SETTINGS_KEY);
    }
    const parsed = JSON.parse(raw);
    const s = state.settings;
    if (THEME_MODES.includes(parsed.theme)) s.theme = parsed.theme;
    if (typeof parsed.flatten === "boolean") s.flatten = parsed.flatten;
    if (typeof parsed.removeMetadata === "boolean") s.removeMetadata = parsed.removeMetadata;
    if (typeof parsed.pageNumbers === "boolean") s.pageNumbers = parsed.pageNumbers;
    if (typeof parsed.watermark === "string") s.watermark = parsed.watermark;
    if (Number.isFinite(parsed.watermarkOpacity)) s.watermarkOpacity = parsed.watermarkOpacity;
    if (Number.isFinite(parsed.watermarkAngle)) s.watermarkAngle = parsed.watermarkAngle;
    if (parsed.info && typeof parsed.info === "object") Object.assign(s.info, parsed.info);
  } catch {
    /* ignore corrupt settings */
  }
}

function persistSettings() {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(state.settings));
  } catch {
    /* ignore */
  }
}

function applyTheme(mode = state.settings.theme, persist = true) {
  const next = THEME_MODES.includes(mode) ? mode : "auto";
  state.settings.theme = next;
  const resolved = next === "auto" ? (systemThemeMedia.matches ? "dark" : "light") : next;
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.dataset.themeMode = next;

  const label = next === "auto" ? `Auto (device: ${resolved})` : next[0].toUpperCase() + next.slice(1);
  const button = document.getElementById("btn-theme");
  if (button) {
    button.title = `Theme: ${label}`;
    button.setAttribute("aria-label", `Theme: ${label}`);
  }
  document.querySelectorAll("#theme-menu button[data-theme]").forEach((item) => {
    item.setAttribute("aria-checked", String(item.dataset.theme === next));
  });

  if (persist) persistSettings();
}

function toggleProtectFields() {
  const enabled = document.getElementById("opt-protect").checked;
  for (const id of [
    "opt-user-password",
    "opt-user-password2",
    "opt-owner-password",
    "perm-print",
    "perm-copy",
    "perm-modify",
    "perm-annotate",
    "perm-assemble",
  ]) {
    const input = document.getElementById(id);
    if (input) input.disabled = !enabled;
  }
  document.getElementById("protect-fields").classList.toggle("disabled", !enabled);
}

function openOptions() {
  const s = state.settings;
  document.getElementById("opt-flatten").checked = !!s.flatten;
  document.getElementById("opt-remove-metadata").checked = !!s.removeMetadata;
  document.getElementById("opt-page-numbers").checked = !!s.pageNumbers;
  document.getElementById("opt-watermark").value = s.watermark;
  document.getElementById("opt-watermark-opacity").value = String(Math.round(s.watermarkOpacity * 100));
  document.getElementById("opt-wm-value").textContent = `${Math.round(s.watermarkOpacity * 100)}%`;
  document.getElementById("opt-watermark-angle").value = String(s.watermarkAngle);
  document.getElementById("opt-wm-angle-value").textContent = `${s.watermarkAngle}°`;
  document.getElementById("meta-title").value = s.info.title || "";
  document.getElementById("meta-author").value = s.info.author || "";
  document.getElementById("meta-subject").value = s.info.subject || "";
  document.getElementById("meta-keywords").value = s.info.keywords || "";

  const security = state.security;
  document.getElementById("opt-protect").checked = !!security.enabled;
  document.getElementById("opt-user-password").value = security.userPassword || "";
  document.getElementById("opt-user-password2").value = security.userPassword || "";
  document.getElementById("opt-owner-password").value = security.ownerPassword || "";
  document.getElementById("perm-print").checked = security.allowPrint !== false;
  document.getElementById("perm-copy").checked = security.allowCopy !== false;
  document.getElementById("perm-modify").checked = !!security.allowModify;
  document.getElementById("perm-annotate").checked = !!security.allowAnnotate;
  document.getElementById("perm-assemble").checked = !!security.allowAssemble;
  toggleProtectFields();
  els.modalOptions.classList.remove("hidden");
}

function applyOptions() {
  const s = state.settings;
  s.flatten = document.getElementById("opt-flatten").checked;
  s.removeMetadata = document.getElementById("opt-remove-metadata").checked;
  s.pageNumbers = document.getElementById("opt-page-numbers").checked;
  s.watermark = document.getElementById("opt-watermark").value;
  s.watermarkOpacity = Number(document.getElementById("opt-watermark-opacity").value) / 100;
  s.watermarkAngle = Number(document.getElementById("opt-watermark-angle").value);
  s.info = {
    title: document.getElementById("meta-title").value,
    author: document.getElementById("meta-author").value,
    subject: document.getElementById("meta-subject").value,
    keywords: document.getElementById("meta-keywords").value,
  };

  if (document.getElementById("opt-protect").checked) {
    const password = document.getElementById("opt-user-password").value;
    const confirmation = document.getElementById("opt-user-password2").value;
    if (!password) {
      alert("Enter an open password, or turn off password protection.");
      return;
    }
    if (password !== confirmation) {
      alert("The open passwords do not match.");
      return;
    }
    state.security = {
      enabled: true,
      userPassword: password,
      ownerPassword: document.getElementById("opt-owner-password").value,
      allowPrint: document.getElementById("perm-print").checked,
      allowCopy: document.getElementById("perm-copy").checked,
      allowModify: document.getElementById("perm-modify").checked,
      allowAnnotate: document.getElementById("perm-annotate").checked,
      allowAssemble: document.getElementById("perm-assemble").checked,
    };
  } else {
    state.security = defaultSecurity();
  }

  persistSettings();
  els.modalOptions.classList.add("hidden");
  setHint("Options applied — they take effect on the next save");
}

/* ------------------------------------------------------------------ *
 * status
 * ------------------------------------------------------------------ */

function updateStatus() {
  const total = state.pages.length;
  const annos = state.pages.reduce((sum, p) => sum + p.annos.length, 0);
  const cropped = state.pages.filter((p) => p.crop).length;
  els.statusPage.textContent = total
    ? `Page ${state.current + 1} of ${total} · ${annos} annotation${annos === 1 ? "" : "s"}${cropped ? ` · ${cropped} cropped` : ""}`
    : "No document";
  els.statusTool.textContent = KIND_LABELS[state.tool] || (state.tool === "select" ? "Select" : state.tool);
  els.zoomLabel.textContent = `${Math.round(state.scale * 100)}%`;
  if (els.empty) els.empty.style.display = total ? "none" : "flex";
  document.getElementById("btn-undo").disabled = !state.history.length;
  document.getElementById("btn-redo").disabled = !state.future.length;
  document.getElementById("btn-delete").disabled = total <= 1;
  document.getElementById("btn-extract").disabled = total === 0;
  const blankButton = document.getElementById("btn-insert-blank");
  blankButton.disabled = false;
  blankButton.title = total
    ? "Insert a blank page after the current page"
    : "Create a blank PDF";
  document.getElementById("btn-split").disabled = total === 0;
  document.getElementById("btn-reverse").disabled = total < 2;
  document.getElementById("btn-clear-crop").disabled = !state.pages[state.current]?.crop;
  document.getElementById("btn-print").disabled = total === 0;
  document.getElementById("btn-save").disabled = total === 0;
  document.getElementById("btn-save-as").disabled = total === 0;
  document.getElementById("btn-sign").disabled = total === 0;
  document.getElementById("btn-add-bookmark").disabled = total === 0;
  document.getElementById("btn-add-comment").disabled = total === 0;
  if (els.bookmarkTitle) {
    els.bookmarkTitle.placeholder = total ? `Bookmark for page ${state.current + 1}` : "Bookmark title";
  }
}

/* ------------------------------------------------------------------ *
 * tools / tabs
 * ------------------------------------------------------------------ */

function setTool(tool) {
  commitOpenEditor();
  hideSelectionMenu();
  state.tool = tool;
  document.body.className = `tool-${tool}`;
  document.querySelectorAll(".tool").forEach((btn) => {
    const active = btn.dataset.tool === tool;
    btn.classList.toggle("active", active);
    btn.setAttribute("aria-pressed", String(active));
  });
  updateStatus();
}

function switchTab(tab) {
  state.activeTab = tab;
  document.querySelectorAll(".tab").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.tab === tab);
  });
  for (const [name, el] of [
    ["thumbs", els.thumbs],
    ["outline", els.outline],
    ["comments", els.comments],
  ]) {
    el.classList.toggle("hidden", name !== tab);
  }
  els.bookmarkTools.classList.toggle("hidden", tab !== "outline");
  els.commentTools.classList.toggle("hidden", tab !== "comments");
  if (tab === "outline") renderOutline();
  if (tab === "comments") renderComments();
}

/* ------------------------------------------------------------------ *
 * wiring
 * ------------------------------------------------------------------ */

document.querySelectorAll(".tool").forEach((btn) => {
  btn.addEventListener("click", () => setTool(btn.dataset.tool));
});

// Clicking anywhere outside an open text box commits it, even if the browser
// moved focus away from the textarea before it could fire a blur event.
document.addEventListener(
  "pointerdown",
  (ev) => {
    document.querySelectorAll(".text-editor").forEach((ta) => {
      if (!ta.contains(ev.target) && ta.__commit) ta.__commit();
    });
  },
  true,
);

// A text selection is captured on pointer release. The snapshot is kept so
// clicking the floating action buttons does not depend on the browser keeping
// the native selection alive after the button receives focus.
document.addEventListener(
  "pointerup",
  (ev) => {
    if (state.tool !== "select" || ev.button !== 0) return;
    const target = ev.target instanceof Element ? ev.target : ev.target?.parentElement;
    if (!target?.closest?.(".textLayer")) return;
    clearTimeout(selectionTimer);
    selectionTimer = setTimeout(captureTextSelection, 0);
  },
  true,
);

document.addEventListener(
  "pointerdown",
  (ev) => {
    if (pendingSelection && !els.selectionMenu.contains(ev.target)) hideSelectionMenu();
    if (commentTarget && !els.commentPopover.contains(ev.target)) commitCommentPopover();
  },
  true,
);

document.addEventListener("keyup", (ev) => {
  if (state.tool !== "select" || !ev.shiftKey || commentTarget) return;
  clearTimeout(selectionTimer);
  selectionTimer = setTimeout(captureTextSelection, 0);
});

els.selectionMenu.addEventListener("pointerdown", (ev) => ev.preventDefault());
document.getElementById("btn-selection-highlight").addEventListener("click", () => highlightSelection(false));
document.getElementById("btn-selection-comment").addEventListener("click", () => highlightSelection(true));
els.commentRemove.addEventListener("click", removeComment);
document.getElementById("btn-comment-cancel").addEventListener("click", closeCommentPopover);
document.getElementById("btn-comment-save").addEventListener("click", commitCommentPopover);
els.commentPopoverText.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    ev.preventDefault();
    closeCommentPopover();
  } else if (ev.key === "Enter" && (ev.ctrlKey || ev.metaKey)) {
    ev.preventDefault();
    commitCommentPopover();
  }
});
els.viewer.addEventListener(
  "scroll",
  () => {
    hideSelectionMenu();
    positionCommentPopover();
  },
  { passive: true },
);
window.addEventListener("resize", () => {
  hideSelectionMenu();
  positionCommentPopover();
});

document.querySelectorAll(".tab").forEach((btn) => {
  btn.addEventListener("click", () => switchTab(btn.dataset.tab));
});

if (els.sidebarResizer) {
  els.sidebarResizer.addEventListener("pointerdown", beginSidebarResize);
  els.sidebarResizer.addEventListener("pointermove", moveSidebarResize);
  els.sidebarResizer.addEventListener("pointerup", endSidebarResize);
  els.sidebarResizer.addEventListener("pointercancel", endSidebarResize);
  els.sidebarResizer.addEventListener("lostpointercapture", endSidebarResize);
  els.sidebarResizer.addEventListener("dblclick", () => {
    setSidebarWidth(SIDEBAR_DEFAULT_WIDTH);
    refreshThumbWidths();
  });
  els.sidebarResizer.addEventListener("keydown", (ev) => {
    const width = els.sidebar.getBoundingClientRect().width;
    if (ev.key === "ArrowLeft" || ev.key === "ArrowRight") {
      ev.preventDefault();
      const step = ev.shiftKey ? 40 : 12;
      setSidebarWidth(width + (ev.key === "ArrowRight" ? step : -step));
      refreshThumbWidths();
    } else if (ev.key === "Home" || ev.key === "End") {
      ev.preventDefault();
      setSidebarWidth(ev.key === "Home" ? SIDEBAR_MIN_WIDTH : sidebarMaxWidth());
      refreshThumbWidths();
    }
  });
}

document.getElementById("btn-open").addEventListener("click", chooseOpen);
document.getElementById("btn-open-empty").addEventListener("click", chooseOpen);
document.getElementById("btn-add").addEventListener("click", chooseAdd);
document.getElementById("btn-save").addEventListener("click", () => saveDocument(false));
document.getElementById("btn-save-as")?.addEventListener("click", () => saveDocument(true));
document.getElementById("btn-undo").addEventListener("click", undo);
document.getElementById("btn-redo").addEventListener("click", redo);
document.getElementById("btn-zoom-in").addEventListener("click", () => setScale(state.scale * 1.2));
document.getElementById("btn-zoom-out").addEventListener("click", () => setScale(state.scale / 1.2));
document.getElementById("btn-fit").addEventListener("click", fitWidth);
document.getElementById("btn-rot-left").addEventListener("click", () => rotatePage(-1));
document.getElementById("btn-rot-right").addEventListener("click", () => rotatePage(1));
document.getElementById("btn-move-up").addEventListener("click", () => movePage(-1));
document.getElementById("btn-move-down").addEventListener("click", () => movePage(1));
document.getElementById("btn-delete").addEventListener("click", deletePage);
document.getElementById("btn-extract").addEventListener("click", extractPage);
document.getElementById("btn-insert-blank").addEventListener("click", insertBlankPage);
document.getElementById("btn-split").addEventListener("click", splitDocument);
document.getElementById("btn-reverse").addEventListener("click", reversePages);
document.getElementById("btn-clear-crop").addEventListener("click", clearCrop);
document.getElementById("btn-sign").addEventListener("click", openSignModal);
document.getElementById("btn-add-bookmark").addEventListener("click", addBookmark);
document.getElementById("btn-add-comment").addEventListener("click", addComment);
els.bookmarkTitle.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") {
    ev.preventDefault();
    addBookmark();
  }
});
els.commentDraft.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") {
    ev.preventDefault();
    addComment();
  }
});
document.getElementById("btn-print").addEventListener("click", printDocument);
document.getElementById("btn-options").addEventListener("click", openOptions);

els.color.addEventListener("input", () => {
  state.color = els.color.value;
  state.colorTouched = true;
});
els.width.addEventListener("input", () => {
  state.strokeWidth = Number(els.width.value);
});

// floating menus (export and theme)
function positionFloatingMenu(button, menu) {
  const margin = 8;
  const btnRect = button.getBoundingClientRect();
  const menuRect = menu.getBoundingClientRect();
  const vw = window.innerWidth;
  const vh = window.innerHeight;

  // Prefer right-aligning the menu to the button, but clamp it to the window.
  let left = btnRect.right - menuRect.width;
  left = Math.max(margin, Math.min(left, vw - menuRect.width - margin));

  // Open below the button unless there is not enough room, then open above.
  let top = btnRect.bottom + 4;
  if (top + menuRect.height > vh - margin) {
    top = Math.max(margin, btnRect.top - menuRect.height - 4);
  }

  menu.style.position = "fixed";
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(top)}px`;
  menu.style.right = "auto";
}

const exportButton = document.getElementById("btn-export");
const themeButton = document.getElementById("btn-theme");

exportButton.addEventListener("click", (ev) => {
  ev.stopPropagation();
  const opening = els.exportMenu.classList.contains("hidden");
  els.themeMenu.classList.add("hidden");
  themeButton.setAttribute("aria-expanded", "false");
  els.exportMenu.classList.toggle("hidden");
  if (opening) positionFloatingMenu(exportButton, els.exportMenu);
});

themeButton.addEventListener("click", (ev) => {
  ev.stopPropagation();
  const opening = els.themeMenu.classList.contains("hidden");
  els.exportMenu.classList.add("hidden");
  els.themeMenu.classList.toggle("hidden");
  themeButton.setAttribute("aria-expanded", String(opening));
  if (opening) positionFloatingMenu(themeButton, els.themeMenu);
});

window.addEventListener("resize", () => {
  if (!els.exportMenu.classList.contains("hidden")) positionFloatingMenu(exportButton, els.exportMenu);
  if (!els.themeMenu.classList.contains("hidden")) positionFloatingMenu(themeButton, els.themeMenu);
  if (els.sidebar) {
    setSidebarWidth(els.sidebar.getBoundingClientRect().width);
    scheduleThumbRefresh();
  }
});

document.querySelectorAll("#export-menu button").forEach((btn) => {
  btn.addEventListener("click", async () => {
    els.exportMenu.classList.add("hidden");
    const kind = btn.dataset.export;
    if (kind === "text") await exportText();
    else if (kind === "png") await exportPng();
    else if (kind === "images") await exportEmbeddedImages();
    else if (kind === "images-pdf") await createPdfFromImages();
  });
});

document.querySelectorAll("#theme-menu button[data-theme]").forEach((item) => {
  item.addEventListener("click", () => {
    applyTheme(item.dataset.theme, true);
    els.themeMenu.classList.add("hidden");
    themeButton.setAttribute("aria-expanded", "false");
  });
});

document.addEventListener("click", () => {
  els.exportMenu.classList.add("hidden");
  els.themeMenu.classList.add("hidden");
  themeButton.setAttribute("aria-expanded", "false");
});

// signature pad
for (const evName of ["pointerdown", "pointermove", "pointerup", "pointercancel", "pointerleave"]) {
  els.signPad.addEventListener(evName, (ev) => {
    if (evName === "pointerdown") {
      signDrawing = true;
      signLast = signPos(ev);
      try {
        els.signPad.setPointerCapture(ev.pointerId);
      } catch {
        /* ignore */
      }
      ev.preventDefault();
    } else if (evName === "pointermove") {
      if (!signDrawing || !signLast) return;
      const next = signPos(ev);
      const ctx = els.signPad.getContext("2d");
      ctx.strokeStyle = "#111";
      ctx.lineWidth = 3;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.beginPath();
      ctx.moveTo(signLast.x, signLast.y);
      ctx.lineTo(next.x, next.y);
      ctx.stroke();
      signLast = next;
      signHasInk = true;
      ev.preventDefault();
    } else {
      signDrawing = false;
      signLast = null;
    }
  });
}
document.getElementById("sign-clear").addEventListener("click", clearSignatureInput);
document.getElementById("sign-cancel").addEventListener("click", closeSignModal);
document.getElementById("sign-use").addEventListener("click", useSignature);
document.querySelectorAll(".sign-tab").forEach((tab) => {
  tab.addEventListener("click", () => setSignMode(tab.dataset.signMode));
});
els.signName.addEventListener("input", drawTypedSignature);
els.signFont.addEventListener("change", drawTypedSignature);
els.signColor.addEventListener("input", drawTypedSignature);
els.signName.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") {
    ev.preventDefault();
    useSignature();
  }
});

// options modal
document.getElementById("options-cancel").addEventListener("click", () => {
  els.modalOptions.classList.add("hidden");
});
document.getElementById("options-apply").addEventListener("click", applyOptions);
document.getElementById("opt-protect").addEventListener("change", toggleProtectFields);
document.getElementById("opt-watermark-opacity").addEventListener("input", (ev) => {
  document.getElementById("opt-wm-value").textContent = `${ev.target.value}%`;
});
document.getElementById("opt-watermark-angle").addEventListener("input", (ev) => {
  document.getElementById("opt-wm-angle-value").textContent = `${ev.target.value}°`;
});

// password prompt
document.getElementById("password-cancel").addEventListener("click", () => closePassword(null));
document.getElementById("password-open").addEventListener("click", submitPassword);
els.passwordInput.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") {
    ev.preventDefault();
    submitPassword();
  } else if (ev.key === "Escape") {
    ev.preventDefault();
    closePassword(null);
  }
});

// search
let searchTimer = null;
els.search.addEventListener("input", () => {
  clearTimeout(searchTimer);
  const value = els.search.value;
  searchTimer = setTimeout(() => runSearch(value), 350);
});
document.getElementById("btn-search-next").addEventListener("click", () => {
  if (state.search.matches.length) gotoMatch(state.search.index + 1);
});
document.getElementById("btn-search-prev").addEventListener("click", () => {
  if (state.search.matches.length) gotoMatch(state.search.index - 1);
});

// track current page while scrolling
let scrollTimer = null;
els.viewer.addEventListener("scroll", () => {
  clearTimeout(scrollTimer);
  scrollTimer = setTimeout(() => {
    if (!state.pages.length) return;
    const top = els.viewer.getBoundingClientRect().top + 80;
    let best = state.current;
    let bestDist = Infinity;
    recs.forEach((rec, i) => {
      if (!rec) return;
      const r = rec.wrap.getBoundingClientRect();
      const dist = Math.abs(r.top - top);
      if (r.bottom > top && dist < bestDist) {
        bestDist = dist;
        best = i;
      }
    });
    if (best !== state.current) selectPage(best);
  }, 120);
});

// keyboard
window.addEventListener("keydown", (ev) => {
  const typing =
    ev.target instanceof HTMLInputElement ||
    ev.target instanceof HTMLTextAreaElement ||
    ev.target?.isContentEditable;
  const mod = ev.ctrlKey || ev.metaKey;

  if (ev.key === "Escape") {
    els.modalSign.classList.add("hidden");
    els.modalOptions.classList.add("hidden");
    els.exportMenu.classList.add("hidden");
    els.themeMenu.classList.add("hidden");
    document.getElementById("btn-theme").setAttribute("aria-expanded", "false");
    if (state.pendingImage && state.tool === "image") {
      state.pendingImage = null;
      setTool("select");
    }
    return;
  }

  if (mod && ev.key.toLowerCase() === "o") {
    ev.preventDefault();
    chooseOpen();
    return;
  }
  if (mod && ev.key.toLowerCase() === "s") {
    ev.preventDefault();
    saveDocument(ev.shiftKey);
    return;
  }
  if (mod && ev.key.toLowerCase() === "p") {
    ev.preventDefault();
    printDocument();
    return;
  }
  if (mod && ev.key.toLowerCase() === "z") {
    ev.preventDefault();
    undo();
    return;
  }
  if (mod && (ev.key.toLowerCase() === "y" || (ev.shiftKey && ev.key.toLowerCase() === "z"))) {
    ev.preventDefault();
    redo();
    return;
  }
  if (mod && ev.key === "0") {
    ev.preventDefault();
    fitWidth();
    return;
  }
  if (mod && (ev.key === "=" || ev.key === "+")) {
    ev.preventDefault();
    setScale(state.scale * 1.2);
    return;
  }
  if (mod && ev.key === "-") {
    ev.preventDefault();
    setScale(state.scale / 1.2);
    return;
  }
  if (typing || mod) return;

  const map = {
    v: "select",
    h: "highlight",
    u: "underline",
    k: "strikeout",
    j: "squiggly",
    p: "ink",
    r: "square",
    t: "text",
    n: "note",
    m: "stamp",
    x: "redact",
    c: "crop",
    e: "erase",
  };
  if (map[ev.key.toLowerCase()]) {
    setTool(map[ev.key.toLowerCase()]);
    return;
  }
  if (ev.key === "ArrowRight" || ev.key === "PageDown" || ev.key === " ") {
    if (state.pages.length) {
      ev.preventDefault();
      selectPageAndReveal(Math.min(state.current + 1, state.pages.length - 1));
    }
    return;
  }
  if (ev.key === "ArrowLeft" || ev.key === "PageUp") {
    if (state.pages.length) {
      ev.preventDefault();
      selectPageAndReveal(Math.max(state.current - 1, 0));
    }
    return;
  }
  if (ev.key === "Delete" || ev.key === "Backspace") {
    if (state.pages.length > 1) {
      ev.preventDefault();
      deletePage();
    }
  }
});

// drag & drop (Tauri v2)
try {
  const webview = window.__TAURI__.webview?.getCurrentWebview?.();
  if (webview?.onDragDropEvent) {
    webview.onDragDropEvent(async (event) => {
      if (event.payload?.type !== "drop") return;
      const paths = event.payload.paths || [];
      const pdf = paths.find((p) => /\.pdf$/i.test(p));
      if (pdf) {
        await openPath(pdf, state.pages.length ? "insert" : "replace");
        return;
      }
      const images = paths.filter((p) => /\.(png|jpe?g)$/i.test(p));
      if (images.length) {
        try {
          const base64 = await invoke("create_pdf_from_images", { paths: images });
          await openBase64Pdf(base64, "Images", state.pages.length ? "insert" : "replace", state.path);
        } catch (err) {
          alert(`Could not create PDF from dropped images:\n${err}`);
        }
      }
    });
  }
} catch (err) {
  console.warn("drag & drop unavailable", err);
}

/* ------------------------------------------------------------------ *
 * boot
 * ------------------------------------------------------------------ */

loadSettings();
applyTheme(state.settings.theme, false);
const onSystemThemeChange = () => {
  if (state.settings.theme === "auto") applyTheme("auto", false);
};
if (typeof systemThemeMedia.addEventListener === "function") {
  systemThemeMedia.addEventListener("change", onSystemThemeChange);
} else if (typeof systemThemeMedia.addListener === "function") {
  systemThemeMedia.addListener(onSystemThemeChange);
}
setTool("select");
switchTab("thumbs");
setSidebarWidth(SIDEBAR_DEFAULT_WIDTH);
updateStatus();
els.color.value = state.color;
els.width.value = String(state.strokeWidth);
setHint("Open a PDF to get started");

// Small debug handle used by the automated UI checks and CDP verification.
window.__pdfed = {
  state,
  recs,
  applyTheme,
  buildPayload,
  openBase64Pdf,
  setTool,
  captureTextSelection,
  highlightSelection,
  openCommentPopover,
  runSearch,
  saveDocument,
  exportText,
  exportPng,
  exportEmbeddedImages,
  chooseOpen,
  chooseAdd,
  insertBlankPage,
  reversePages,
  splitDocument,
  extractPage,
  createPdfFromImages,
};
