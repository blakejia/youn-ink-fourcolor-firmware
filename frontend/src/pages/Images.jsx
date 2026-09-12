import React, { useEffect, useRef, useState } from 'react';
import { api } from '../api.js';
import { getSelectedDevice } from '../deviceContext.js';

export default function Images() {
  const [pages, setPages] = useState([]);
  const [device, setDevice] = useState(getSelectedDevice());
  const [page, setPage] = useState('');
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');
  const fileRef = useRef(null);
  const [previewUrl, setPreviewUrl] = useState(null);

  const load = async () => {
    const dev = getSelectedDevice();
    if (!dev) { setPages([]); setPage(''); setErr(''); return; }
    try {
      const p = await api.pages(dev);
      setPages(p || []);
      setErr('');
    } catch (e) { setErr(e.message); }
  };
  useEffect(() => {
    const sync = () => { setDevice(getSelectedDevice()); };
    window.addEventListener('device-changed', sync);
    return () => window.removeEventListener('device-changed', sync);
  }, []);
  useEffect(() => { setPage(''); load(); }, [device]);

  const onPick = (e) => {
    const f = e.target.files[0];
    if (!f) return;
    setPreviewUrl(URL.createObjectURL(f));
  };

  const push = async () => {
    setErr(''); setOk('');
    const dev = getSelectedDevice();
    if (!dev) { setErr('请先选择设备'); return; }
    const f = fileRef.current?.files?.[0];
    if (!f) { setErr('请选择图片'); return; }
    if (!page) { setErr('请选择目标页面'); return; }
    try {
      await api.uploadToPage(f, page, dev);
      setOk(`已替换页面「${page}」，设备将在下一轮轮询后更新（该页 md5 已变化）`);
      if (fileRef.current) fileRef.current.value = '';
      setPreviewUrl(null);
    } catch (e) { setErr(e.message); }
  };

  return (
    <div>
      <h1>替换页面画面</h1>
      {err && <div className="err">{err}</div>}
      {ok && <div className="ok">{ok}</div>}

      <div className="card">
        <h2>上传替换</h2>
        {!device && <p className="muted">请先选择设备</p>}
        <div className="row">
          <label>目标页面
            <select value={page} onChange={(e) => setPage(e.target.value)} disabled={!device}>
              <option value="">请选择页面</option>
              {pages.map((p) => (
                <option key={p.name} value={p.name}>{p.name}</option>
              ))}
            </select>
          </label>
          <input ref={fileRef} type="file" accept="image/*" onChange={onPick} disabled={!device} />
          <button className="btn" onClick={push} disabled={!device || !page}>上传并替换</button>
        </div>
        {previewUrl && (
          <div style={{ marginTop: 12 }}>
            <p className="muted">本地预览：</p>
            <img src={previewUrl} style={{ maxHeight: 120 }} alt="本地预览" />
          </div>
        )}
      </div>
    </div>
  );
}
