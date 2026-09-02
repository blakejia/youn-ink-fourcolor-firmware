import React, { useEffect, useState } from 'react';
import { api } from '../api.js';

export default function Devices() {
  const [devices, setDevices] = useState([]);
  const [err, setErr] = useState('');
  const [pairing, setPairing] = useState(null); // {code, device_id}
  const [pairCode, setPairCode] = useState('');
  const [newDeviceId, setNewDeviceId] = useState('');
  const [newBoard, setNewBoard] = useState('zectrix-s3-epaper-4.2');

  const load = async () => {
    try { setDevices(await api.devices()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  useEffect(() => { load(); }, []);

  const startPair = async () => {
    if (!newDeviceId.trim()) { setErr('请输入 device_id（或读设备日志获取）'); return; }
    try {
      const r = await api.pairStart(newDeviceId.trim(), newBoard);
      setPairing({ code: r.code, device_id: newDeviceId.trim() });
      setPairCode('');
      setErr('');
    } catch (e) { setErr(e.message); }
  };

  const confirmPair = async () => {
    try {
      await api.pairConfirm(pairing.device_id, pairCode.trim());
      setPairing(null);
      load();
    } catch (e) { setErr(e.message); }
  };

  return (
    <div>
      <h1>设备管理</h1>
      {err && <div className="err">{err}</div>}

      <div className="card">
        <h2>配对新设备</h2>
        <div className="row">
          <input placeholder="device_id" value={newDeviceId} onChange={(e) => setNewDeviceId(e.target.value)} />
          <select value={newBoard} onChange={(e) => setNewBoard(e.target.value)}>
            <option value="zectrix-s3-epaper-4.2">zectrix-s3-epaper-4.2 (NOTE4C)</option>
          </select>
          <button className="btn" onClick={startPair}>获取配对码</button>
        </div>
        {pairing && (
          <div style={{ marginTop: 12 }}>
            <p>配对码已生成：<b className="mono">{pairing.code}</b>（有效期 300 秒）</p>
            <p className="muted">查看设备屏幕上的 6 位码，输入确认：</p>
            <div className="row">
              <input value={pairCode} onChange={(e) => setPairCode(e.target.value)} placeholder="6 位码" maxLength={6} />
              <button className="btn" onClick={confirmPair}>确认配对</button>
            </div>
          </div>
        )}
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
