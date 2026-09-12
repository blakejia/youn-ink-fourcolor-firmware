import React, { useEffect, useState } from 'react';
import { useBlocker } from 'react-router-dom';
import { api, apiFetch } from '../api.js';
import { getSelectedDevice } from '../deviceContext.js';
import { Banner, BusyButton, ConfirmDialog } from '../ui.jsx';
import { registerUnsavedCheck } from '../unsaved.js';
import CanvasEditor from '../CanvasEditor.jsx';

const EMPTY_CANVAS = JSON.stringify(
  { default: [{ type: 'div', props: { tw: 'flex flex-col p-[12px] gap-[8px] bg-white', style: { color: '#000000' }, children: '新页面' } }] },
  null, 2
);

export default function Pages() {
  const [pages, setPages] = useState([]);
  const [device, setDevice] = useState(getSelectedDevice());
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');
  const [editing, setEditing] = useState(null); // {name, canvas_json, duration_minutes, order}
  const [name, setName] = useState('');
  const [duration, setDuration] = useState(10);
  const [order, setOrder] = useState(0);
  const [json, setJson] = useState(EMPTY_CANVAS);
  const [dirty, setDirty] = useState(false); // unsaved canvas edits
  const [busy, setBusy] = useState('');

  const load = async () => {
    const dev = getSelectedDevice();
    if (!dev) { setPages([]); setErr(''); return; }
    try {
      const p = await api.pages(dev);
      if (getSelectedDevice() !== dev) return;
      setPages(p); setErr('');
    }
    catch (e) { if (getSelectedDevice() === dev) setErr(e.message); }
  };
  useEffect(() => {
    const sync = () => { setDevice(getSelectedDevice()); };
    window.addEventListener('device-changed', sync);
    return () => window.removeEventListener('device-changed', sync);
  }, []);
  useEffect(() => { setEditing(null); setDirty(false); setErr(''); setOk(''); load(); }, [device]);

  // Losing a half-built canvas is silent and expensive, so guard in-app
  // navigation with a dialog and a reload/tab-close with beforeunload.
  const blocker = useBlocker(dirty);
  useEffect(() => {
    if (!dirty) return;
    const warn = (e) => { e.preventDefault(); e.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);
  // The device selector lives in the layout; let it ask before it discards this.
  useEffect(() => {
    registerUnsavedCheck(() => dirty);
    return () => registerUnsavedCheck(null);
  }, [dirty]);

  const startNew = () => {
    setEditing({});
    setName(''); setDuration(10); setOrder(0); setJson(EMPTY_CANVAS);
    setOk(''); setErr(''); setDirty(true);
  };

  const startEdit = (p) => {
    setEditing(p);
    setName(p.name);
    setDuration(p.duration_minutes);
    setOrder(p.order);
    setJson(JSON.stringify(p.canvas_json, null, 2));
    setOk(''); setErr(''); setDirty(false);
  };

  const save = async () => {
    setErr(''); setOk('');
    const dev = getSelectedDevice();
    if (!dev) { setErr('请先选择设备'); return; }
    let canvas;
    try { canvas = JSON.parse(json); }
    catch (e) { setErr('JSON 解析失败：' + e.message); return; }
    setBusy('save');
    try {
      await api.createPage({ name: name.trim(), device: dev, canvas_json: canvas, duration_minutes: Number(duration), order: Number(order) });
      setDirty(false);
      setOk('已保存');
      setEditing(null);
      await load();
    } catch (e) { setErr(e.message); }
    finally { setBusy(''); }
  };

  const del = async (nm) => {
    if (!confirm(`删除页 ${nm}？该页面将从设备的轮播中移除。`)) return;
    setBusy('del:' + nm);
    setErr('');
    try {
      await api.deletePage(nm, getSelectedDevice());
      if (editing?.name === nm) { setEditing(null); setDirty(false); }
      await load();
    } catch (e) { setErr(e.message); }
    finally { setBusy(''); }
  };

  return (
    <div>
      <h1>页组管理</h1>
      <Banner>{err}</Banner>
      <Banner kind="ok">{ok}</Banner>

      <div className="card">
        <div className="card-head">
          <h2>页组列表</h2>
          <button type="button" className="btn" onClick={startNew} disabled={!device}>新建页</button>
          <BusyButton
            className="btn secondary"
            busy={busy === 'load'}
            busyText="刷新中…"
            onClick={async () => { setBusy('load'); try { await load(); } finally { setBusy(''); } }}
            disabled={!device}
          >
            刷新
          </BusyButton>
        </div>
        {!device ? (
          <p className="muted">请先选择设备</p>
        ) : pages.length === 0 ? (
          <p className="muted">该设备名下还没有页面，点「新建页」开始。</p>
        ) : (
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">名称</th>
                  <th scope="col">时长(min)</th>
                  <th scope="col">顺序</th>
                  <th scope="col">MD5</th>
                  <th scope="col">操作</th>
                </tr>
              </thead>
              <tbody>
                {pages.map((p) => (
                  <tr key={p.name}>
                    <td className="wrap-anywhere">{p.name}</td>
                    <td className="num">{p.duration_minutes}</td>
                    <td className="num">{p.order}</td>
                    <td className="mono wrap-anywhere" title={p.md5 || '尚未渲染'}>{p.md5 || '—'}</td>
                    <td>
                      <div className="row" style={{ gap: 6, flexWrap: 'nowrap' }}>
                        <button type="button" className="btn secondary" onClick={() => startEdit(p)} disabled={!device}>编辑</button>
                        <BusyButton
                          className="btn danger"
                          busy={busy === 'del:' + p.name}
                          busyText="删除中…"
                          onClick={() => del(p.name)}
                          disabled={!device}
                        >
                          删除
                        </BusyButton>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {editing !== null && (
        <div className="card">
          <h2>{editing.name ? `编辑页：${editing.name}` : '新建页'}</h2>
          <div className="row" style={{ marginBottom: 12, alignItems: 'flex-end' }}>
            <label className="field" style={{ flex: '1 1 220px' }}>
              <div className="field-label">名称</div>
              <input
                name="page_name"
                value={name}
                onChange={(e) => { setName(e.target.value); setDirty(true); }}
                placeholder="如 logo-1024…"
                autoComplete="off"
                spellCheck={false}
              />
            </label>
            <label className="field" style={{ flex: '0 0 110px' }}>
              <div className="field-label">时长（分钟）</div>
              <input
                name="page_duration"
                type="number"
                min={1}
                value={duration}
                onChange={(e) => { setDuration(e.target.value); setDirty(true); }}
              />
            </label>
            <label className="field" style={{ flex: '0 0 100px' }}>
              <div className="field-label">顺序</div>
              <input
                name="page_order"
                type="number"
                value={order}
                onChange={(e) => { setOrder(e.target.value); setDirty(true); }}
              />
            </label>
          </div>
          <CanvasEditor
            canvasJson={json}
            onChange={(parsed) => { setJson(JSON.stringify(parsed, null, 2)); setDirty(true); }}
            operatorFetch={apiFetch}
          />
          <div className="row" style={{ marginTop: 10 }}>
            <BusyButton busy={busy === 'save'} busyText="保存中…" onClick={save} disabled={!device}>
              保存
            </BusyButton>
            <button
              type="button"
              className="btn secondary"
              onClick={() => { setEditing(null); setDirty(false); }}
            >
              取消
            </button>
            {dirty && <span className="muted">未保存</span>}
          </div>
        </div>
      )}

      {blocker.state === 'blocked' && (
        <ConfirmDialog
          title="有未保存的修改"
          body="离开本页会丢失尚未保存的画布修改。"
          confirmLabel="放弃修改并离开"
          cancelLabel="留在本页"
          danger
          onConfirm={() => blocker.proceed()}
          onCancel={() => blocker.reset()}
        />
      )}
    </div>
  );
}
