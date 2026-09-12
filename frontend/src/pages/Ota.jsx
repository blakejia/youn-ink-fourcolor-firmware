import React, { useEffect, useRef, useState } from 'react';
import { api } from '../api.js';
import { Banner, BusyButton } from '../ui.jsx';
import { formatBytes } from '../format.js';

export default function Ota() {
  const fileRef = useRef(null);
  const [version, setVersion] = useState('');
  const [channel, setChannel] = useState('stable');
  const [notes, setNotes] = useState('');
  const [check, setCheck] = useState(null);
  const [err, setErr] = useState('');
  const [ok, setOk] = useState('');
  const [busy, setBusy] = useState('');

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
    setBusy('upload');
    try {
      const r = await api.uploadOta(f, version.trim(), channel, notes);
      setOk(`已上传 ${r.version}（${formatBytes(r.size)}）。`);
      setVersion('');
      setNotes('');
      if (fileRef.current) fileRef.current.value = '';
      await loadCheck();
    } catch (e) { setErr(e.message); }
    finally { setBusy(''); }
  };

  return (
    <div>
      <h1>OTA 管理</h1>
      <Banner>{err}</Banner>
      <Banner kind="ok">{ok}</Banner>

      <div className="card">
        <h2>上传固件</h2>
        <p className="muted">上传的镜像会成为设备检查更新时拿到的最新固件。</p>
        <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
          <label className="field" style={{ flex: '1 1 220px' }}>
            <div className="field-label">固件文件（.bin）</div>
            <input ref={fileRef} type="file" name="firmware" accept=".bin" disabled={busy === 'upload'} />
          </label>
          <label className="field" style={{ flex: '0 0 190px' }}>
            <div className="field-label">版本号</div>
            <input
              name="version"
              value={version}
              onChange={(e) => setVersion(e.target.value)}
              placeholder="如 6.5.9-note4c…"
              autoComplete="off"
              spellCheck={false}
              disabled={busy === 'upload'}
            />
          </label>
          <label className="field" style={{ flex: '0 0 120px' }}>
            <div className="field-label">通道</div>
            <select name="channel" value={channel} onChange={(e) => setChannel(e.target.value)} disabled={busy === 'upload'}>
              <option value="stable">stable</option>
              <option value="beta">beta</option>
            </select>
          </label>
          <label className="field" style={{ flex: '1 1 180px' }}>
            <div className="field-label">备注</div>
            <input
              name="notes"
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              placeholder="可选，如 修复翻页卡顿…"
              autoComplete="off"
              disabled={busy === 'upload'}
            />
          </label>
          <BusyButton busy={busy === 'upload'} busyText="上传中…" onClick={upload}>
            上传
          </BusyButton>
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <h2>当前最新固件</h2>
          <BusyButton
            className="btn secondary"
            busy={busy === 'check'}
            busyText="刷新中…"
            onClick={async () => { setBusy('check'); try { await loadCheck(); } finally { setBusy(''); } }}
          >
            刷新
          </BusyButton>
        </div>
        {check && check.available ? (
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">版本</th>
                  <th scope="col">大小</th>
                  <th scope="col">SHA-256</th>
                  <th scope="col">通道</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>{check.version}</td>
                  <td className="num">{formatBytes(check.size)}</td>
                  <td className="mono wrap-anywhere" title={check.sha256}>{check.sha256}</td>
                  <td>{check.channel || 'stable'}</td>
                </tr>
              </tbody>
            </table>
          </div>
        ) : (
          <p className="muted">暂无已上传固件</p>
        )}
      </div>
    </div>
  );
}
