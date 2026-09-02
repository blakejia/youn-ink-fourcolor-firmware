import React, { useCallback, useEffect, useRef, useState } from 'react';

// Canvas editor: server-rendered preview + selectable element overlay.
//
// Layout is computed ONLY on the server (canvas_render.py). The editor
// POSTs the canvas JSON with ?debug=1 and gets back a PNG + the bounding
// box of every rendered node. Clicking a rectangle selects the element and
// shows its editable properties in the right panel.

const SCREEN_W = 400;
const SCREEN_H = 300;

// ─── JSON path get/set helpers ───
// Path shape: "windowData.default[0].props.children[1]" or "...props.children" (text)

function parsePath(path) {
  // → [{key:'windowData'},{key:'default',index:0},{key:'props'},{key:'children',index:1}]
  const out = [];
  const re = /([a-zA-Z_$][\w$]*)|\[(\d+)\]/g;
  let m;
  while ((m = re.exec(path))) {
    if (m[1] !== undefined) out.push({ key: m[1] });
    else out.push({ index: parseInt(m[2], 10) });
  }
  return out;
}

function getAtPath(root, path) {
  let cur = root;
  for (const seg of parsePath(path)) {
    if (cur == null) return undefined;
    cur = seg.index !== undefined ? cur[seg.index] : cur[seg.key];
  }
  return cur;
}

function setAtPath(root, path, value) {
  const segs = parsePath(path);
  if (segs.length === 0) return root;
  const clone = Array.isArray(root) ? [...root] : { ...root };
  let curNew = clone;
  let curOld = root;
  for (let i = 0; i < segs.length - 1; i++) {
    const seg = segs[i];
    const nextOld = seg.index !== undefined ? curOld[seg.index] : curOld[seg.key];
    const nextNew = Array.isArray(nextOld) ? [...nextOld] : { ...nextOld };
    if (seg.index !== undefined) {
      curNew[seg.index] = nextNew;
      curOld = curOld[seg.index];
    } else {
      curNew[seg.key] = nextNew;
      curOld = curOld[seg.key];
    }
    curNew = nextNew;
  }
  const last = segs[segs.length - 1];
  if (last.index !== undefined) curNew[last.index] = value;
  else curNew[last.key] = value;
  return clone;
}

// Mutate one property of the element at `path`. Handles both node paths
// (windowData.default[0]) and text pseudo-paths (windowData.default[0].props.children).
function updateElement(canvasJson, path, mutate) {
  const node = getAtPath(canvasJson, path);
  // For text pseudo-path, node is the text string; the parent node is one level up.
  const isTextPseudo = typeof node === 'string';
  const nodePath = isTextPseudo ? path.replace(/\.props\.children.*$/, '') : path;
  const parentNode = getAtPath(canvasJson, nodePath);
  if (!parentNode || typeof parentNode !== 'object') return canvasJson;
  const nextNode = mutate(structuredClone(parentNode), isTextPseudo);
  return setAtPath(canvasJson, nodePath, nextNode);
}

// ─── tw helpers ───

function twSetSize(tw, axis, px) {
  const tokens = String(tw || '').split(/\s+/).filter(Boolean);
  const re = new RegExp(`^${axis}-\\[\\d+px\\]$`);
  const next = tokens.filter(t => !re.test(t));
  next.push(`${axis}-[${px}px]`);
  return next.join(' ');
}

function twSetFontSize(tw, px) {
  const tokens = String(tw || '').split(/\s+/).filter(Boolean);
  const re = /^text-\[\d+px\]$/;
  const next = tokens.filter(t => !re.test(t));
  next.push(`text-[${px}px]`);
  return next.join(' ');
}

function twGetFontSize(tw, style) {
  const m = String(tw || '').match(/text-\[(\d+)px\]/);
  if (m) return parseInt(m[1], 10);
  if (style?.fontSize) {
    const v = parseInt(String(style.fontSize).replace('px', ''), 10);
    if (!isNaN(v)) return v;
  }
  return 16;
}

function twGetBg(tw) {
  const m = String(tw || '').match(/(?:^|\s)bg-(white|black|red|yellow)(?:\s|$)/);
  return m ? m[1] : 'white';
}

function twSetBg(tw, name) {
  const tokens = String(tw || '').split(/\s+/).filter(Boolean);
  const re = /^bg-(white|black|red|yellow)$/;
  const next = tokens.filter(t => !re.test(t));
  next.push(`bg-${name}`);
  return next.join(' ');
}

// ─── Component ───

export default function CanvasEditor({ canvasJson, onChange, operatorFetch }) {
  const [bounds, setBounds] = useState([]);
  const [pngUrl, setPngUrl] = useState(null);
  const [selectedPath, setSelectedPath] = useState(null);
  const [renderErr, setRenderErr] = useState('');
  const debounceRef = useRef(null);
  const pngUrlRef = useRef(null);

  // Debounced server render
  useEffect(() => {
    clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(async () => {
      try {
        const parsed = typeof canvasJson === 'string' ? JSON.parse(canvasJson) : canvasJson;
        const resp = await operatorFetch('/pages/preview?debug=1', {
          method: 'POST',
          body: { canvas_json: parsed },
        });
        setBounds(resp.bounds || []);
        if (pngUrlRef.current) URL.revokeObjectURL(pngUrlRef.current);
        const bin = Uint8Array.from(atob(resp.png_b64), c => c.charCodeAt(0));
        const url = URL.createObjectURL(new Blob([bin], { type: 'image/png' }));
        pngUrlRef.current = url;
        setPngUrl(url);
        setRenderErr('');
      } catch (e) {
        setRenderErr(e.message || String(e));
        setBounds([]);
      }
    }, 350);
    return () => clearTimeout(debounceRef.current);
  }, [canvasJson, operatorFetch]);

  useEffect(() => () => {
    if (pngUrlRef.current) URL.revokeObjectURL(pngUrlRef.current);
  }, []);

  const parsed = (() => {
    try { return typeof canvasJson === 'string' ? JSON.parse(canvasJson) : canvasJson; }
    catch { return null; }
  })();

  // Server bounds paths are prefixed "windowData.default[...]"; the JSON the
  // editor holds IS the windowData object (top level key is "default").
  const strip = (p) => p.replace(/^windowData\./, '');

  const selected = bounds.find(b => b.path === selectedPath) || null;
  const selectedNode = parsed && selected ? getAtPath(parsed, strip(selected.path)) : null;
  const isTextPseudo = typeof selectedNode === 'string';
  const actualNode = isTextPseudo
    ? getAtPath(parsed, strip(selected.path).replace(/\.props\.children(\[\d+\])?$/, ''))
    : selectedNode;

  const applyMutate = useCallback((mutate) => {
    if (!parsed || !selected) return;
    const next = updateElement(parsed, strip(selected.path), mutate);
    onChange(next);
  }, [parsed, selected, onChange]);
  return (
    <div style={{ display: 'flex', gap: 16, alignItems: 'flex-start' }}>
      {/* Canvas */}
      <div
        style={{
          width: SCREEN_W,
          height: SCREEN_H,
          position: 'relative',
          border: '1px solid var(--color-border, #ccd2da)',
          background: '#fff',
          flexShrink: 0,
          imageRendering: 'pixelated',
        }}
      >
        {pngUrl && (
          <img
            src={pngUrl}
            alt="canvas preview"
            style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', imageRendering: 'pixelated' }}
            draggable={false}
          />
        )}
        {bounds.map(b => (
          <div
            key={b.path + ':' + b.x + ',' + b.y}
            onClick={() => setSelectedPath(b.path)}
            title={b.path}
            style={{
              position: 'absolute',
              left: b.x, top: b.y, width: b.w, height: b.h,
              cursor: 'pointer',
              outline: selectedPath === b.path ? '2px solid var(--color-primary, #2f6feb)' : '1px dashed rgba(47,111,235,0.35)',
              outlineOffset: -1,
              background: selectedPath === b.path ? 'rgba(47,111,235,0.06)' : 'transparent',
            }}
          />
        ))}
        {renderErr && (
          <div style={{
            position: 'absolute', inset: 0, display: 'flex', alignItems: 'center', justifyContent: 'center',
            background: 'rgba(255,255,255,0.85)', color: 'var(--color-danger, #d9363e)',
            fontSize: 12, padding: 12, textAlign: 'center',
          }}>
            渲染失败：{renderErr}
          </div>
        )}
      </div>

      {/* Property panel */}
      <div style={{ width: 280, flexShrink: 0 }}>
        <h3 style={{ margin: '0 0 12px', fontSize: 14 }}>属性面板</h3>
        {!selected && <div className="muted">点击画布元素查看属性</div>}
        {selected && actualNode && (
          <PropertyForm
            bound={selected}
            node={actualNode}
            isTextPseudo={isTextPseudo}
            onMutate={applyMutate}
          />
        )}
      </div>
    </div>
  );
}

function PropertyForm({ bound, node, isTextPseudo, onMutate }) {
  const props = node.props || {};
  const tw = String(props.tw || '');
  const style = props.style || {};
  const children = props.children;

  const setTw = (fn) => onMutate(n => { n.props = n.props || {}; n.props.tw = fn(n.props.tw); return n; });
  const setStyle = (key, value) => onMutate(n => {
    n.props = n.props || {};
    n.props.style = { ...(n.props.style || {}), [key]: value };
    return n;
  });

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10, fontSize: 13 }}>
      <div>
        <div className="muted" style={{ marginBottom: 4 }}>路径</div>
        <div className="mono" style={{ fontSize: 11, wordBreak: 'break-all' }}>{bound.path}</div>
      </div>
      <div>
        <div className="muted" style={{ marginBottom: 4 }}>类型</div>
        <div><span className="badge off" style={{ background: 'var(--color-soft, #eef1f5)', color: 'inherit' }}>{bound.type}</span></div>
      </div>

      {/* text content */}
      {typeof children === 'string' && (
        <label style={{ display: 'block' }}>
          <div className="muted" style={{ marginBottom: 4 }}>文本内容</div>
          <textarea
            value={children}
            onChange={(e) => onMutate(n => { n.props = n.props || {}; n.props.children = e.target.value; return n; })}
            style={{ width: '100%', minHeight: 56, fontFamily: 'inherit' }}
          />
        </label>
      )}

      {/* font size (text-bearing) */}
      {(typeof children === 'string' || bound.type === 'text') && (
        <label style={{ display: 'block' }}>
          <div className="muted" style={{ marginBottom: 4 }}>字号 (px)</div>
          <input
            type="number" min={8} max={64}
            value={twGetFontSize(tw, style)}
            onChange={(e) => setTw(t => twSetFontSize(t, parseInt(e.target.value || '16', 10)))}
            style={{ width: '100%' }}
          />
        </label>
      )}

      {/* text color */}
      {(typeof children === 'string' || bound.type === 'text') && (
        <label style={{ display: 'block' }}>
          <div className="muted" style={{ marginBottom: 4 }}>文字颜色</div>
          <select
            value={style.color || '#000000'}
            onChange={(e) => setStyle('color', e.target.value)}
            style={{ width: '100%' }}
          >
            <option value="#000000">黑</option>
            <option value="#FFFFFF">白</option>
            <option value="#FFD700">黄</option>
            <option value="#DC1E1E">红</option>
          </select>
        </label>
      )}

      {/* img src + size */}
      {bound.type === 'img' && (
        <>
          <label style={{ display: 'block' }}>
            <div className="muted" style={{ marginBottom: 4 }}>图片 src</div>
            <input
              value={props.src || ''}
              onChange={(e) => onMutate(n => { n.props = n.props || {}; n.props.src = e.target.value; return n; })}
              placeholder="data:image/...;base64,... 或 https://..."
              style={{ width: '100%', fontSize: 11, fontFamily: 'ui-monospace,monospace' }}
            />
          </label>
          <div style={{ display: 'flex', gap: 8 }}>
            <label style={{ flex: 1 }}>
              <div className="muted" style={{ marginBottom: 4 }}>宽 (px)</div>
              <input
                type="number" min={1} max={SCREEN_W}
                value={bound.w}
                onChange={(e) => setTw(t => twSetSize(t, 'w', parseInt(e.target.value || '0', 10)))}
                style={{ width: '100%' }}
              />
            </label>
            <label style={{ flex: 1 }}>
              <div className="muted" style={{ marginBottom: 4 }}>高 (px)</div>
              <input
                type="number" min={1} max={SCREEN_H}
                value={bound.h}
                onChange={(e) => setTw(t => twSetSize(t, 'h', parseInt(e.target.value || '0', 10)))}
                style={{ width: '100%' }}
              />
            </label>
          </div>
        </>
      )}

      {/* bg color for div/span */}
      {(bound.type === 'div' || bound.type === 'span') && (
        <label style={{ display: 'block' }}>
          <div className="muted" style={{ marginBottom: 4 }}>背景色</div>
          <select value={twGetBg(tw)} onChange={(e) => setTw(t => twSetBg(t, e.target.value))} style={{ width: '100%' }}>
            <option value="white">白</option>
            <option value="black">黑</option>
            <option value="yellow">黄</option>
            <option value="red">红</option>
          </select>
        </label>
      )}

      {/* raw tw */}
      <label style={{ display: 'block' }}>
        <div className="muted" style={{ marginBottom: 4 }}>tw（高级，直接编辑）</div>
        <textarea
          value={tw}
          onChange={(e) => onMutate(n => { n.props = n.props || {}; n.props.tw = e.target.value; return n; })}
          className="mono"
          style={{ width: '100%', minHeight: 60, fontSize: 11 }}
        />
      </label>
    </div>
  );
}
