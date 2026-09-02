import React, { useEffect, useRef, useState } from 'react';
// ─── Canvas layout engine (mirrors server-side canvas_render.py) ───

const SCREEN_W = 400;
const SCREEN_H = 300;

const PALETTE = {
  black: [0, 0, 0],
  white: [255, 255, 255],
  yellow: [255, 215, 0],
  red: [220, 30, 30],
};

function parsePx(token) {
  if (!token) return null;
  const m = token.match(/\[(-?\d+)px\]/);
  return m ? parseInt(m[1]) : null;
}

function parsePadding(style, tw) {
  let pt = 0, pr = 0, pb = 0, pl = 0;
  if (style.paddingTop) pt = parseInt(style.paddingTop);
  if (style.paddingRight) pr = parseInt(style.paddingRight);
  if (style.paddingBottom) pb = parseInt(style.paddingBottom);
  if (style.paddingLeft) pl = parseInt(style.paddingLeft);
  for (const tok of tw.split(' ')) {
    if (tok === 'p') { pt = pr = pb = pl = parsePx(tok) || 0; }
    else if (tok.startsWith('p-[')) { pt = pr = pb = pl = parsePx(tok) || 0; }
    else if (tok.startsWith('px-[')) { pl = pr = parsePx(tok) || 0; }
    else if (tok.startsWith('py-[')) { pt = pb = parsePx(tok) || 0; }
  }
  return [pt, pr, pb, pl];
}

function parseMargin(style, tw) {
  let mt = 0, mr = 0, mb = 0, ml = 0;
  for (const tok of tw.split(' ')) {
    if (tok === 'm') { mt = mr = mb = ml = parsePx(tok) || 0; }
    else if (tok.startsWith('mx-[')) { ml = mr = parsePx(tok) || 0; }
    else if (tok.startsWith('my-[')) { mt = mb = parsePx(tok) || 0; }
  }
  return [mt, mr, mb, ml];
}

function direction(tw) {
  if (tw.includes('flex-row')) return 'row';
  if (tw.includes('flex-col')) return 'column';
  return 'row';
}

function alignItems(tw) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith('items-')) return tok.slice(6);
  }
  return 'stretch';
}

function justify(tw) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith('justify-')) return tok.slice(8);
  }
  return 'flex-start';
}

function bgColor(tw, style) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith('bg-')) {
      const name = tok.slice(3);
      if (name in PALETTE) return name;
    }
  }
  if (style.backgroundColor) {
    const hex = style.backgroundColor;
    if (hex.startsWith('#')) {
      const r = parseInt(hex.slice(1, 3), 16);
      const g = parseInt(hex.slice(3, 5), 16);
      const b = parseInt(hex.slice(5, 7), 16);
      let best = 'white';
      let bestDist = Infinity;
      for (const [name, [pr, pg, pb]] of Object.entries(PALETTE)) {
        const dist = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2;
        if (dist < bestDist) { bestDist = dist; best = name; }
      }
      return best;
    }
  }
  return null;
}

function borderColor(tw, style) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith('border-') && tok !== 'border') {
      const name = tok.slice(7);
      if (name in PALETTE) return name;
    }
  }
  return null;
}

function radius(tw, style) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith('rounded-')) {
      const v = parsePx(tok);
      if (v) return v;
    }
  }
  if (style.borderRadius) {
    try { return parseInt(String(style.borderRadius).replace('px', '')); } catch { return 0; }
  }
  return 0;
}

function hasBorder(tw, style) {
  return tw.split(' ').includes('border') || !!style.border;
}

function sizeFromTw(tw, axis) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith(`${axis}-[`)) return parsePx(tok);
  }
  return null;
}

function resolveFontSize(tw, style) {
  for (const tok of tw.split(' ')) {
    if (tok.startsWith('text-[') && tok.endsWith('px]')) {
      return parsePx(tok) || 16;
    }
  }
  if (style.fontSize) {
    try { return parseInt(String(style.fontSize).replace('px', '')); } catch { return 16; }
  }
  return 16;
}

function stylePx(v) {
  if (v == null) return null;
  if (typeof v === 'number') return v;
  const m = String(v).match(/(-?\d+)/);
  return m ? parseInt(m[1]) : null;
}

// Measure a node (returns [w, h])
function measureNode(node, parentTw) {
  const props = node.props || {};
  const nodeTw = String(props.tw || '');
  const style = props.style || {};
  const w = sizeFromTw(nodeTw, 'w') || stylePx(style.width) || sizeFromTw(parentTw, 'w');
  const h = sizeFromTw(nodeTw, 'h') || stylePx(style.height) || sizeFromTw(parentTw, 'h');
  if (w && h) return [w, h];
  if (node.type === 'img') return [w || 0, h || 0];
  return [w || 100, h || 24];
}

// Resolve children (returns array of [kind, value] where kind is 'text' or 'node')
function resolveChildren(node) {
  const children = (node.props || {}).children;
  if (children == null) return [];
  if (typeof children === 'string') return [['text', children]];
  if (typeof children === 'object' && !Array.isArray(children)) return [['node', children]];
  if (Array.isArray(children)) {
    return children.map(c => {
      if (typeof c === 'string') return ['text', c];
      if (typeof c === 'object') return ['node', c];
      return ['text', String(c)];
    });
  }
  return [];
}

// Layout algorithm: returns array of {x, y, w, h, path, node, kind}
function layoutChildren(children, tw, style, box, parentPath) {
  const [pt, pr, pb, pl] = parsePadding(style, tw);
  const inner = {
    x: box.x + pl,
    y: box.y + pt,
    w: Math.max(0, box.w - pl - pr),
    h: Math.max(0, box.h - pt - pb),
  };
  if (inner.w <= 0 || inner.h <= 0) return [];

  const dir = direction(tw);
  const gap = parsePx(tw) || 0;
  const align = alignItems(tw);
  const just = justify(tw);

  const measurements = children.map(([kind, val]) => {
    if (kind === 'text') {
      const fontSize = resolveFontSize(tw, style);
      // Approximate text measurement (same as server: use font size as height)
      return [kind, val, fontSize * val.length * 0.6, fontSize];
    }
    const [w, h] = measureNode(val, tw);
    return [kind, val, w, h];
  });

  const result = [];
  if (dir === 'row') {
    const totalW = measurements.reduce((s, [, , w]) => s + w, 0) + gap * Math.max(0, measurements.length - 1);
    let x = justifyOffset(just, inner.x, inner.w, totalW, gap, measurements.length);
    for (let i = 0; i < measurements.length; i++) {
      const [kind, val, w, h] = measurements[i];
      const y = inner.y + alignOffset(align, inner.h, h);
      result.push({ x, y, w, h, kind, val, path: `${parentPath}.children[${i}]` });
      x += w + gap;
    }
  } else {
    const totalH = measurements.reduce((s, [, , , h]) => s + h, 0) + gap * Math.max(0, measurements.length - 1);
    let y = justifyOffset(just, inner.y, inner.h, totalH, gap, measurements.length);
    for (let i = 0; i < measurements.length; i++) {
      const [kind, val, w, h] = measurements[i];
      const x = inner.x + alignOffset(align, inner.w, w);
      result.push({ x, y, w, h, kind, val, path: `${parentPath}.children[${i}]` });
      y += h + gap;
    }
  }
  return result;
}

function alignOffset(align, containerSize, childSize) {
  if (childSize >= containerSize) return 0;
  if (align === 'center' || align === 'items-center') return Math.floor((containerSize - childSize) / 2);
  if (align === 'end' || align === 'flex-end' || align === 'items-end') return containerSize - childSize;
  return 0;
}

function justifyOffset(just, origin, containerSize, totalSize, gap, count) {
  if (count === 0) return origin;
  const free = containerSize - totalSize;
  if (just === 'center' || just === 'justify-center') return origin + Math.max(0, Math.floor(free / 2));
  if (just === 'between' || just === 'space-between' || just === 'justify-between') return origin;
  if (just === 'end' || just === 'flex-end' || just === 'justify-end') return origin + Math.max(0, free);
  return origin;
}

// Render canvas to a flat list of positioned elements
export function renderCanvas(canvasJson) {
  const defaultList = canvasJson.default || [];
  const elements = [];

  function renderNode(node, box, path) {
    const ntype = node.type;
    if (!['div', 'span', 'img'].includes(ntype)) return;

    const props = node.props || {};
    const tw = String(props.tw || '');
    const style = props.style || {};

    const bg = bgColor(tw, style);
    const border = hasBorder(tw, style);
    const borderCol = borderColor(tw, style) || 'black';
    const rad = radius(tw, style);

    if (ntype === 'img') {
      elements.push({
        type: 'img',
        x: box.x, y: box.y, w: box.w, h: box.h,
        path,
        node,
        src: props.src || '',
      });
      return;
    }

    elements.push({
      type: ntype,
      x: box.x, y: box.y, w: box.w, h: box.h,
      path,
      node,
      bg,
      border,
      borderCol,
      rad,
    });

    const children = resolveChildren(node);
    if (children.length === 0) return;

    if (children.every(([k]) => k === 'text')) {
      // Text-only: render as a single text element
      const text = children.map(([, v]) => v).join('');
      const fontSize = resolveFontSize(tw, style);
      elements.push({
        type: 'text',
        x: box.x, y: box.y, w: box.w, h: box.h,
        path: `${path}.children`,
        node,
        text,
        fontSize,
        color: style.color || '#000000',
      });
      return;
    }

    const laid = layoutChildren(children, tw, style, box, path);
    for (const item of laid) {
      if (item.kind === 'text') {
        const fontSize = resolveFontSize(tw, style);
        elements.push({
          type: 'text',
          x: item.x, y: item.y, w: item.w, h: item.h,
          path: item.path,
          node,
          text: item.val,
          fontSize,
          color: style.color || '#000000',
        });
      } else {
        renderNode(item.val, { x: item.x, y: item.y, w: item.w, h: item.h }, item.path);
      }
    }
  }

  for (let i = 0; i < defaultList.length; i++) {
    renderNode(defaultList[i], { x: 0, y: 0, w: SCREEN_W, h: SCREEN_H }, `windowData.default[${i}]`);
  }
  return elements;
}

// ─── CanvasEditor component ───

export default function CanvasEditor({ canvasJson, onChange }) {
  const canvasRef = useRef(null);
  const [selectedPath, setSelectedPath] = useState(null);
  const [dragState, setDragState] = useState(null);
  const [elements, setElements] = useState([]);

  // Re-render canvas when canvasJson changes
  useEffect(() => {
    try {
      const parsed = typeof canvasJson === 'string' ? JSON.parse(canvasJson) : canvasJson;
      setElements(renderCanvas(parsed));
    } catch (e) {
      setElements([]);
    }
  }, [canvasJson]);

  const handleMouseDown = (e, elem) => {
    e.stopPropagation();
    setSelectedPath(elem.path);
    if (elem.type !== 'img') return; // only img can be dragged/resized

    const rect = canvasRef.current.getBoundingClientRect();
    const startX = e.clientX - rect.left;
    const startY = e.clientY - rect.top;
    setDragState({
      elem,
      startX,
      startY,
      startW: elem.w,
      startH: elem.h,
      mode: 'move', // or 'resize'
    });
  };

  const handleMouseMove = (e) => {
    if (!dragState) return;
    const rect = canvasRef.current.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const dx = x - dragState.startX;
    const dy = y - dragState.startY;

    if (dragState.mode === 'move') {
      // Move: update x/y (not implemented — img position is determined by flex layout)
      // For now, only resize is supported
    } else if (dragState.mode === 'resize') {
      const newW = Math.max(10, dragState.startW + dx);
      const newH = Math.max(10, dragState.startH + dy);
      // Update the img's width/height in the JSON
      updateImageSize(dragState.elem.path, newW, newH);
    }
  };

  const handleMouseUp = () => {
    setDragState(null);
  };

  const updateImageSize = (path, w, h) => {
    const parsed = typeof canvasJson === 'string' ? JSON.parse(canvasJson) : JSON.parse(JSON.stringify(canvasJson));
    const keys = path.replace(/^windowData\.default\[/, '').replace(/\]/g, '').split('.');
    let current = parsed.default[parseInt(keys[0])];
    for (let i = 1; i < keys.length; i++) {
      if (keys[i] === 'children') {
        current = current.props.children;
      } else if (keys[i].startsWith('[')) {
        current = current[parseInt(keys[i].slice(1, -1))];
      } else {
        current = current[keys[i]];
      }
    }
    if (current.props) {
      current.props.style = { ...current.props.style, width: `${w}px`, height: `${h}px` };
      onChange(parsed);
    }
  };

  const selectedElement = elements.find(e => e.path === selectedPath);

  return (
    <div style={{ display: 'flex', gap: 16 }}>
      {/* Canvas */}
      <div
        ref={canvasRef}
        style={{
          width: SCREEN_W,
          height: SCREEN_H,
          position: 'relative',
          border: '1px solid #ccc',
          background: '#fff',
          overflow: 'hidden',
          cursor: dragState ? 'grabbing' : 'default',
        }}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
      >
        {elements.map((elem) => (
          <div
            key={elem.path}
            onMouseDown={(e) => handleMouseDown(e, elem)}
            style={{
              position: 'absolute',
              left: elem.x,
              top: elem.y,
              width: elem.w,
              height: elem.h,
              background: elem.bg ? PALETTE[elem.bg] : 'transparent',
              border: elem.border ? `1px solid ${PALETTE[elem.borderCol] || '#000'}` : 'none',
              borderRadius: elem.rad || 0,
              cursor: elem.type === 'img' ? 'grab' : 'default',
              outline: selectedPath === elem.path ? '2px solid #2f6feb' : 'none',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              overflow: 'hidden',
            }}
          >
            {elem.type === 'text' && (
              <span style={{
                fontSize: elem.fontSize,
                color: elem.color,
                whiteSpace: 'nowrap',
              }}>
                {elem.text}
              </span>
            )}
            {elem.type === 'img' && (
              <img
                src={elem.src}
                alt=""
                style={{ width: '100%', height: '100%', objectFit: 'contain' }}
                draggable={false}
              />
            )}
          </div>
        ))}
      </div>

      {/* Property panel */}
      <div style={{ width: 280, flexShrink: 0 }}>
        <h3 style={{ margin: '0 0 12px', fontSize: 14 }}>属性面板</h3>
        {selectedElement ? (
          <div>
            <div className="muted" style={{ marginBottom: 8 }}>{selectedElement.path}</div>
            <div style={{ marginBottom: 8 }}>
              <label>类型</label>
              <input value={selectedElement.type} readOnly style={{ width: '100%' }} />
            </div>
            {selectedElement.type === 'text' && (
              <>
                <div style={{ marginBottom: 8 }}>
                  <label>文本</label>
                  <textarea
                    value={selectedElement.text}
                    onChange={(e) => {
                      const parsed = typeof canvasJson === 'string' ? JSON.parse(canvasJson) : JSON.parse(JSON.stringify(canvasJson));
                      const keys = selectedElement.path.replace(/^windowData\.default\[/, '').replace(/\]/g, '').split('.');
                      let current = parsed.default[parseInt(keys[0])];
                      for (let i = 1; i < keys.length; i++) {
                        if (keys[i] === 'children') {
                          current = current.props.children;
                        } else if (keys[i].startsWith('[')) {
                          current = current[parseInt(keys[i].slice(1, -1))];
                        } else {
                          current = current[keys[i]];
                        }
                      }
                      if (typeof current === 'string') {
                        // Text node: replace the string
                        const parentPath = selectedElement.path.replace(/\.children$/, '');
                        const parentKeys = parentPath.replace(/^windowData\.default\[/, '').replace(/\]/g, '').split('.');
                        let parent = parsed.default[parseInt(parentKeys[0])];
                        for (let i = 1; i < parentKeys.length; i++) {
                          if (parentKeys[i] === 'children') {
                            parent = parent.props.children;
                          } else if (parentKeys[i].startsWith('[')) {
                            parent = parent[parseInt(parentKeys[i].slice(1, -1))];
                          } else {
                            parent = parent[parentKeys[i]];
                          }
                        }
                        parent.props.children = e.target.value;
                        onChange(parsed);
                      }
                    }}
                    style={{ width: '100%', minHeight: 60 }}
                  />
                </div>
                <div style={{ marginBottom: 8 }}>
                  <label>字号</label>
                  <input
                    type="number"
                    value={selectedElement.fontSize}
                    onChange={(e) => {
                      const parsed = typeof canvasJson === 'string' ? JSON.parse(canvasJson) : JSON.parse(JSON.stringify(canvasJson));
                      const keys = selectedElement.path.replace(/^windowData\.default\[/, '').replace(/\]/g, '').split('.');
                      let current = parsed.default[parseInt(keys[0])];
                      for (let i = 1; i < keys.length; i++) {
                        if (keys[i] === 'children') {
                          current = current.props.children;
                        } else if (keys[i].startsWith('[')) {
                          current = current[parseInt(keys[i].slice(1, -1))];
                        } else {
                          current = current[keys[i]];
                        }
                      }
                      if (current.props) {
                        current.props.tw = current.props.tw.replace(/text-\[\d+px\]/, `text-[${e.target.value}px]`);
                        onChange(parsed);
                      }
                    }}
                    style={{ width: '100%' }}
                  />
                </div>
                <div style={{ marginBottom: 8 }}>
                  <label>颜色</label>
                  <select
                    value={selectedElement.color}
                    onChange={(e) => {
                      const parsed = typeof canvasJson === 'string' ? JSON.parse(canvasJson) : JSON.parse(JSON.stringify(canvasJson));
                      const keys = selectedElement.path.replace(/^windowData\.default\[/, '').replace(/\]/g, '').split('.');
                      let current = parsed.default[parseInt(keys[0])];
                      for (let i = 1; i < keys.length; i++) {
                        if (keys[i] === 'children') {
                          current = current.props.children;
                        } else if (keys[i].startsWith('[')) {
                          current = current[parseInt(keys[i].slice(1, -1))];
                        } else {
                          current = current[keys[i]];
                        }
                      }
                      if (current.props) {
                        current.props.style = { ...current.props.style, color: e.target.value };
                        onChange(parsed);
                      }
                    }}
                    style={{ width: '100%' }}
                  >
                    <option value="#000000">黑色</option>
                    <option value="#FFFFFF">白色</option>
                    <option value="#FFD700">黄色</option>
                    <option value="#DC1E1E">红色</option>
                  </select>
                </div>
              </>
            )}
            {selectedElement.type === 'img' && (
              <>
                <div style={{ marginBottom: 8 }}>
                  <label>宽度</label>
                  <input
                    type="number"
                    value={selectedElement.w}
                    onChange={(e) => updateImageSize(selectedElement.path, parseInt(e.target.value), selectedElement.h)}
                    style={{ width: '100%' }}
                  />
                </div>
                <div style={{ marginBottom: 8 }}>
                  <label>高度</label>
                  <input
                    type="number"
                    value={selectedElement.h}
                    onChange={(e) => updateImageSize(selectedElement.path, selectedElement.w, parseInt(e.target.value))}
                    style={{ width: '100%' }}
                  />
                </div>
              </>
            )}
            {selectedElement.type === 'div' && (
              <div className="muted">div 是容器，宽高由子元素决定，不支持拖拽</div>
            )}
          </div>
        ) : (
          <div className="muted">点击画布元素查看属性</div>
        )}
      </div>
    </div>
  );
}
