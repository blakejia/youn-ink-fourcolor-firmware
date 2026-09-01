import React, { useEffect, useRef, useState } from 'react';
import { api } from '../api.js';

export default function Ota() {
  const fileRef = useRef(null);
  const [version, setVersion] = useState('');
  const [channel, setChannel] = useState('stable');
  const [notes, setNotes] = useState('');
  const [check, setCheck] = useState(null);
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');

  const loadCheck = async () => {
    try { setCheck(await api.otaCheck()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  useEffect(() => { loadCheck(); }, []);

  const upload = async () => {
    setErr(''); setOk('');
    const f = fileRef.current?.files?.[0];
    if (!f) { setErr('请选择固件 .bin'); return; }
    if (!version.trim()) { setErr('请输入版本号'); return; }
    try {
      const r = await api.uploadOta(f, version.trim(), channel, notes);
      setOk(`已上传 ${r.version} (${r.size} bytes)`);
      setVersion('');
      loadCheck();
    } catch (e) { setErr(e.message); }
  };

  return (
    <div>
      <h1>OTA 管理</h1>
      {err && <div className="err">{err}</div>}
      {ok && <div className="ok">{ok}</div>}

      <div className="card">
        <h2>上传固件</h2>
        <div className="row">
          <input ref={fileRef} type="file" accept=".bin" />
          <input placeholder="版本号（如 6.5.9-note4c）" value={version} onChange={(e) => setVersion(e.target.value)} />
          <label>通道
            <select value={channel} onChange={(e) => setChannel(e.target.value)}>
              <option value="stable">stable</option>
              <option value="beta">beta</option>
            </select>
          </label>
          <input placeholder="备注" value={notes} onChange={(e) => setNotes(e.target.value)} />
          <button className="btn" onClick={upload}>上传</button>
        </div>
      </div>

      <div className="card">
        <h2>当前最新固件 <button className="btn secondary" onClick={loadCheck}>刷新</button></h2>
        {check && check.available ? (
          <table>
            <thead><tr><th>版本</th><th>大小</th><th>SHA-256</th><th>通道</th></tr></thead>
            <tbody>
              <tr>
                <td>{check.version}</td>
                <td>{check.size}</td>
                <td className="mono">{check.sha256}</td>
                <td>{check.channel || 'stable'}</td>
              </tr>
            </tbody>
          </table>
        ) : (
          <p className="muted">暂无已上传固件</p>
        )}
      </div>
    </div>
  );
}
