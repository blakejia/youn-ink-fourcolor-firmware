import React, { useEffect, useState } from 'react';
import { api } from '../api.js';
import CanvasEditor from '../CanvasEditor.jsx';
const EMPTY_CANVAS = JSON.stringify(
  { default: [{ type: 'div', props: { tw: 'flex flex-col p-[12px] gap-[8px] bg-white', style: { color: '#000000' }, children: '新页面' } }] },
  null, 2
);

export default function Pages() {
  const [pages, setPages] = useState([]);
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');
  const [editing, setEditing] = useState(null); // {name, canvas_json, duration_minutes, order}
  const [name, setName] = useState('');
  const [duration, setDuration] = useState(10);
  const [order, setOrder] = useState(0);
  const [json, setJson] = useState(EMPTY_CANVAS);
  const [preview, setPreview] = useState(null); // object URL
  const [previewErr, setPreviewErr] = useState('');

  const load = async () => {
    try { setPages(await api.pages()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  useEffect(() => { load(); }, []);

  const startNew = () => {
    setEditing({});
    setName(''); setDuration(10); setOrder(0); setJson(EMPTY_CANVAS); setPreview(null);
  };

  const startEdit = (p) => {
    setEditing(p);
    setName(p.name);
    setDuration(p.duration_minutes);
    setOrder(p.order);
    setJson(JSON.stringify(p.canvas_json, null, 2));
    setPreview(null);
  };

  const doPreview = async () => {
    setPreviewErr('');
    try {
      const canvas = JSON.parse(json);
      const blob = await api.preview(canvas);
      setPreview(URL.createObjectURL(blob));
    } catch (e) { setPreviewErr(e.message); }
  };

  const save = async () => {
    setErr(''); setOk('');
    let canvas;
    try { canvas = JSON.parse(json); }
    catch (e) { setErr('JSON 解析失败: ' + e.message); return; }
    try {
      await api.createPage({ name: name.trim(), canvas_json: canvas, duration_minutes: Number(duration), order: Number(order) });
      setOk('已保存');
      setEditing(null);
      load();
    } catch (e) { setErr(e.message); }
  };

  const del = async (nm) => {
    if (!confirm(`删除页 ${nm}?`)) return;
    try { await api.deletePage(nm); load(); }
    catch (e) { setErr(e.message); }
  };

  return (
    <div>
      <h1>页组管理</h1>
      {err && <div className="err">{err}</div>}
      {ok && <div className="ok">{ok}</div>}

      <div className="card">
        <div className="row">
          <h2 style={{ flex: 1, margin: 0 }}>页组列表</h2>
          <button className="btn" onClick={startNew}>新建页</button>
          <button className="btn secondary" onClick={load}>刷新</button>
        </div>
        {pages.length === 0 ? (
          <p className="muted">暂无页（或未登录）</p>
        ) : (
          <table>
            <thead><tr><th>名称</th><th>时长(min)</th><th>顺序</th><th>MD5</th><th>操作</th></tr></thead>
            <tbody>
              {pages.map((p) => (
                <tr key={p.name}>
                  <td>{p.name}</td>
                  <td>{p.duration_minutes}</td>
                  <td>{p.order}</td>
                  <td className="mono">{p.canvas_json ? p.name : p.name}</td>
                  <td>
                    <button className="btn secondary" onClick={() => startEdit(p)}>编辑</button>{' '}
                    <button className="btn danger" onClick={() => del(p.name)}>删除</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {editing !== null && (
        <div className="card">
          <h2>{editing.name ? `编辑页: ${editing.name}` : '新建页'}</h2>
          <div className="row" style={{ marginBottom: 12 }}>
            <input placeholder="名称" value={name} onChange={(e) => setName(e.target.value)} />
            <label>时长(min) <input type="number" value={duration} onChange={(e) => setDuration(e.target.value)} style={{ width: 70 }} /></label>
            <label>顺序 <input type="number" value={order} onChange={(e) => setOrder(e.target.value)} style={{ width: 60 }} /></label>
          </div>
          <CanvasEditor canvasJson={json} onChange={(parsed) => setJson(JSON.stringify(parsed, null, 2))} />
          {previewErr && <div className="err">{previewErr}</div>}
          <div className="row" style={{ marginTop: 10 }}>
            <button className="btn secondary" onClick={doPreview}>预览位图</button>
            <button className="btn" onClick={save}>保存</button>
            <button className="btn secondary" onClick={() => setEditing(null)}>取消</button>
          </div>
          {preview && (
            <div style={{ marginTop: 12 }}>
              <p className="muted">服务端渲染预览（400×300 2bpp）：</p>
              <img className="preview" src={preview} width="400" height="300" alt="canvas preview" />
            </div>
          )}

        </div>
      )}
    </div>
  );
}
