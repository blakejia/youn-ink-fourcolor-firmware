import React, { useEffect, useState } from 'react';
import { api } from '../api.js';

export default function Devices() {
  const [devices, setDevices] = useState([]);
  const [err, setErr] = useState('');
  const [pending, setPending] = useState([]); // [{device_id, code, expires_in}]
  const [pairCode, setPairCode] = useState('');
  const [manualDeviceId, setManualDeviceId] = useState('');

  const load = async () => {
    try { setDevices(await api.devices()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  const loadPending = async () => {
    try { setPending(await api.pairPending()); }
    catch (e) { setErr(e.message); }
  };
  useEffect(() => { load(); loadPending(); }, []);

  // One-click confirm for a device-initiated session (code shown on device screen).
  const confirmPair = async (deviceId, code) => {
    try {
      await api.pairConfirm(deviceId, code);
      setPairCode('');
      setManualDeviceId('');
      loadPending();
      load();
    } catch (e) { setErr(e.message); }
  };

  // Manual fallback: type the 6-digit code read from the device screen.
  const confirmManual = async () => {
    const target = pending.length === 1 ? pending[0].device_id : manualDeviceId.trim();
    if (!target) { setErr('无待确认设备，请先刷新或填写 device_id'); return; }
    if (!pairCode.trim()) { setErr('请输入设备屏幕上的 6 位码'); return; }
    confirmPair(target, pairCode.trim());
  };

  return (
    <div>
      <h1>设备管理</h1>
      {err && <div className="err">{err}</div>}

      <div className="card">
        <h2>待确认配对 <button className="btn secondary" onClick={loadPending}>刷新</button></h2>
        <p className="muted">设备开机联网后自动发起配对，屏幕显示 6 位码后在此确认。</p>
        {pending.length === 0 ? (
          <p className="muted">暂无待确认设备</p>
        ) : (
          <table>
            <thead><tr><th>Device ID</th><th>配对码</th><th>剩余秒</th><th>操作</th></tr></thead>
            <tbody>
              {pending.map((s) => (
                <tr key={s.device_id}>
                  <td className="mono">{s.device_id}</td>
                  <td className="mono"><b>{s.code}</b></td>
                  <td className="muted">{s.expires_in}</td>
                  <td>
                    <button className="btn" onClick={() => confirmPair(s.device_id, s.code)}>确认配对</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <div className="row" style={{ marginTop: 12 }}>
          <input value={manualDeviceId} onChange={(e) => setManualDeviceId(e.target.value)} placeholder="device_id（多台待确认时填）" />
          <input value={pairCode} onChange={(e) => setPairCode(e.target.value)} placeholder="6 位码（手动核对）" maxLength={6} />
          <button className="btn secondary" onClick={confirmManual}>手动确认</button>
        </div>
      </div>

      <div className="card">
        <h2>设备列表 <button className="btn secondary" onClick={load}>刷新</button></h2>
        {devices.length === 0 ? (
          <p className="muted">暂无设备</p>
        ) : (
          <table>
            <thead><tr><th>Device ID</th><th>板型</th><th>首次/最后在线</th><th>信任</th><th>操作</th></tr></thead>
            <tbody>
              {devices.map((d) => (
                <tr key={d.device_id}>
                  <td className="mono">{d.device_id}</td>
                  <td>{d.board_type}</td>
                  <td className="muted">{d.last_seen}</td>
                  <td><span className={`badge ${d.trust ? 'on' : 'off'}`}>{d.trust ? '已信任' : '未信任'}</span></td>
                  <td>
                    {d.trust
                      ? <button className="btn danger" onClick={async () => { await api.revoke(d.device_id); load(); }}>吊销</button>
                      : <button className="btn" onClick={async () => { await api.approve(d.device_id); load(); }}>信任</button>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
