import React, { useEffect, useRef, useState } from 'react';
import { api } from '../api.js';
import { getSelectedDevice } from '../deviceContext.js';
import { Banner, BusyButton } from '../ui.jsx';

export default function Images() {
  const [pages, setPages] = useState([]);
  const [device, setDevice] = useState(getSelectedDevice());
  const [page, setPage] = useState('');
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');
  const [busy, setBusy] = useState(false);
  const fileRef = useRef(null);
  const [previewUrl, setPreviewUrl] = useState(null);
  const previewUrlRef = useRef(null);

  // Object URLs pin the file in memory until revoked; every new pick and the
  // unmount must release the previous one.
  const setPreview = (url) => {
    if (previewUrlRef.current) URL.revokeObjectURL(previewUrlRef.current);
    previewUrlRef.current = url;
    setPreviewUrl(url);
  };
  useEffect(() => () => {
    if (previewUrlRef.current) URL.revokeObjectURL(previewUrlRef.current);
  }, []);

  const load = async () => {
    const dev = getSelectedDevice();
    if (!dev) { setPages([]); setPage(''); setErr(''); return; }
    try {
      const p = await api.pages(dev);
      if (getSelectedDevice() !== dev) return;
      setPages(p || []);
      setErr('');
    } catch (e) { if (getSelectedDevice() === dev) setErr(e.message); }
  };
  useEffect(() => {
    const sync = () => { setDevice(getSelectedDevice()); };
    window.addEventListener('device-changed', sync);
    return () => window.removeEventListener('device-changed', sync);
  }, []);
  useEffect(() => { setPage(''); setErr(''); setOk(''); setPreview(null); load(); }, [device]);

  const onPick = (e) => {
    const f = e.target.files[0];
    if (!f) return;
    setPreview(URL.createObjectURL(f));
  };

  const push = async () => {
    setErr(''); setOk('');
    const dev = getSelectedDevice();
    if (!dev) { setErr('请先选择设备'); return; }
    const f = fileRef.current?.files?.[0];
    if (!f) { setErr('请选择图片'); return; }
    if (!page) { setErr('请选择目标页面'); return; }
    setBusy(true);
    try {
      await api.uploadToPage(f, page, dev);
      setOk(`已替换页面「${page}」，设备将在下一轮轮询后更新。`);
      if (fileRef.current) fileRef.current.value = '';
      setPreview(null);
    } catch (e) { setErr(e.message); }
    finally { setBusy(false); }
  };

  return (
    <div>
      <h1>替换页面画面</h1>
      <Banner>{err}</Banner>
      <Banner kind="ok">{ok}</Banner>

      <div className="card">
        <h2>上传替换</h2>
        <p className="muted">上传的图片会成为该页的画面，原来的画布内容被替换。</p>
        {!device && <p className="muted">请先选择设备</p>}
        <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
          <label className="field" style={{ flex: '0 0 200px' }}>
            <div className="field-label">目标页面</div>
            <select name="target_page" value={page} onChange={(e) => setPage(e.target.value)} disabled={!device || busy}>
              <option value="">请选择页面</option>
              {pages.map((p) => (
                <option key={p.name} value={p.name}>{p.name}</option>
              ))}
            </select>
          </label>
          <label className="field" style={{ flex: '1 1 220px' }}>
            <div className="field-label">图片文件</div>
            <input
              ref={fileRef}
              type="file"
              name="image"
              accept="image/*"
              onChange={onPick}
              disabled={!device || busy}
            />
          </label>
          <BusyButton busy={busy} busyText="上传中…" onClick={push} disabled={!device || !page}>
            上传并替换
          </BusyButton>
        </div>
        {previewUrl && (
          <div style={{ marginTop: 12 }}>
            <p className="muted">本地预览</p>
            {/* Fixed-height box so the image landing does not shift the layout. */}
            <div style={{ height: 120, display: 'flex', alignItems: 'center', marginTop: 6 }}>
              <img
                src={previewUrl}
                alt="所选图片的本地预览"
                width={400}
                height={300}
                style={{ maxHeight: 120, width: 'auto', maxWidth: '100%', imageRendering: 'pixelated', border: '1px solid var(--color-border)', borderRadius: 4 }}
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
