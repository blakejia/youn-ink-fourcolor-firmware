import React, { useEffect, useRef, useState } from 'react';
import { api } from '../api.js';

export default function Images() {
  const [devices, setDevices] = useState([]);
  const [images, setImages] = useState([]);
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');
  const fileRef = useRef(null);
  const [origUrl, setOrigUrl] = useState(null);
  const [previewUrl, setPreviewUrl] = useState(null);
  const [format, setFormat] = useState('bwry2bpp');
  const [title, setTitle] = useState('');
  const [target, setTarget] = useState('');

  const load = async () => {
    try {
      const [d, imgs] = await Promise.all([api.devices(), api.images()]);
      setDevices(d || []);
      setImages(imgs || []);
      setErr('');
    } catch (e) { setErr(e.message); }
  };
  useEffect(() => { load(); }, []);

  const onPick = (e) => {
    const f = e.target.files[0];
    if (!f) return;
    setOrigUrl(URL.createObjectURL(f));
    setPreviewUrl(null);
  };

  const push = async () => {
    setErr(''); setOk('');
    const f = fileRef.current?.files?.[0];
    if (!f) { setErr('请选择图片'); return; }
    try {
      await api.uploadImage(f, format, title, target);
      setOk('上传成功，已推送（若目标在线）');
      load();
    } catch (e) { setErr(e.message); }
  };

  return (
    <div>
      <h1>图片推送</h1>
      {err && <div className="err">{err}</div>}
      {ok && <div className="ok">{ok}</div>}

      <div className="card">
        <h2>上传图片</h2>
        <div className="row">
          <input ref={fileRef} type="file" accept="image/*" onChange={onPick} />
          <label>格式
            <select value={format} onChange={(e) => setFormat(e.target.value)}>
              <option value="bwry2bpp">2bpp 四色 BWRY</option>
              <option value="1bpp">1bpp 黑白</option>
            </select>
          </label>
          <input placeholder="标题" value={title} onChange={(e) => setTitle(e.target.value)} />
          <label>目标设备
            <select value={target} onChange={(e) => setTarget(e.target.value)}>
              <option value="">所有在线设备</option>
              {devices.filter((d) => d.trust).map((d) => (
                <option key={d.device_id} value={d.device_id}>{d.device_id}</option>
              ))}
            </select>
          </label>
          <button className="btn" onClick={push}>上传并推送</button>
        </div>
        {origUrl && (
          <div style={{ marginTop: 12 }}>
            <p className="muted">原图：</p>
            <img src={origUrl} style={{ maxHeight: 120 }} alt="原图" />
            {previewUrl && (
              <>
                <p className="muted">转换预览：</p>
                <img className="preview" src={previewUrl} width="400" height="300" alt="转换" />
              </>
            )}
          </div>
        )}
      </div>

      <div className="card">
        <h2>已上传图片 <button className="btn secondary" onClick={load}>刷新</button></h2>
        {images.length === 0 ? (
          <p className="muted">暂无图片</p>
        ) : (
          <table>
            <thead><tr><th>ID</th><th>标题</th><th>格式</th><th>大小</th><th>操作</th></tr></thead>
            <tbody>
              {images.map((im) => (
                <tr key={im.id}>
                  <td className="mono">{im.id}</td>
                  <td>{im.title}</td>
                  <td>{im.format}</td>
                  <td>{im.size}</td>
                  <td><button className="btn danger" onClick={async () => { await api.deleteImage(im.id); load(); }}>删除</button></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
