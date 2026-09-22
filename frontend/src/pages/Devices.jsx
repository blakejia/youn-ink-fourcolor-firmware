import React, { useEffect, useState } from 'react';
import { api } from '../api.js';
import { Banner, BusyButton } from '../ui.jsx';
import { formatCountdown, formatTime, timeAgo } from '../format.js';

export default function Devices() {
  const [devices, setDevices] = useState([]);
  const [err, setErr] = useState('');
  const [pending, setPending] = useState([]); // [{device_id, code, expires_in}]
  const [pairCode, setPairCode] = useState('');
  const [manualDeviceId, setManualDeviceId] = useState('');
  // Key of the action currently in flight, so the right button reports it.
  const [busy, setBusy] = useState('');

  const fetchDevices = async () => {
    try { setDevices(await api.devices()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  const fetchPending = async () => {
    try { setPending(await api.pairPending()); setErr(''); }
    catch (e) { setErr(e.message); }
  };

  const refresh = async (key, fn) => {
    setBusy(key);
    setErr('');
    try { await fn(); } finally { setBusy(''); }
  };

  useEffect(() => { fetchDevices(); fetchPending(); }, []);

  // One-click confirm for a device-initiated session (code shown on device screen).
  const confirmPair = async (deviceId, code, key) => {
    setBusy(key);
    setErr('');
    try {
      await api.pairConfirm(deviceId, code);
      setPairCode('');
      setManualDeviceId('');
      await Promise.all([fetchPending(), fetchDevices()]);
    } catch (e) { setErr(e.message); }
    finally { setBusy(''); }
  };

  // Manual fallback: type the 6-digit code read from the device screen.
  const confirmManual = () => {
    const target = pending.length === 1 ? pending[0].device_id : manualDeviceId.trim();
    if (!target) { setErr('无待确认设备，请先刷新或填写 device_id'); return; }
    if (!pairCode.trim()) { setErr('请输入设备屏幕上的 6 位码'); return; }
    confirmPair(target, pairCode.trim(), 'pair:manual');
  };

  const setTrust = async (device, trust) => {
    // Revoking trust locks the device out until it pairs again, so it is not
    // something to do on a stray click.
    if (!trust && !confirm(`吊销 ${device.device_id} 的信任？该设备需要重新配对。`)) return;
    setBusy('trust:' + device.device_id);
    setErr('');
    try {
      if (trust) await api.approve(device.device_id);
      else await api.revoke(device.device_id);
      await fetchDevices();
    } catch (e) { setErr(e.message); }
    finally { setBusy(''); }
  };

  return (
    <div>
      <h1>设备管理</h1>
      <Banner>{err}</Banner>

      <div className="card">
        <div className="card-head">
          <h2>待确认配对</h2>
          <BusyButton
            className="btn secondary"
            busy={busy === 'pending'}
            busyText="刷新中…"
            onClick={() => refresh('pending', fetchPending)}
          >
            刷新
          </BusyButton>
        </div>
        <p className="muted">设备开机联网后自动发起配对，屏幕显示 6 位码后在此确认。</p>
        {pending.length === 0 ? (
          <p className="muted" style={{ marginTop: 8 }}>暂无待确认设备</p>
        ) : (
          <div className="table-wrap" style={{ marginTop: 8 }}>
            <table>
              <thead>
                <tr>
                  <th scope="col">Device ID</th>
                  <th scope="col">配对码</th>
                  <th scope="col">剩余有效</th>
                  <th scope="col">操作</th>
                </tr>
              </thead>
              <tbody>
                {pending.map((s) => (
                  <tr key={s.device_id}>
                    <td className="mono wrap-anywhere">{s.device_id}</td>
                    <td className="mono num" style={{ fontSize: 14, letterSpacing: 1 }}><b>{s.code}</b></td>
                    <td className="num">{formatCountdown(s.expires_in)}</td>
                    <td>
                      <BusyButton
                        busy={busy === 'pair:' + s.device_id}
                        busyText="确认中…"
                        onClick={() => confirmPair(s.device_id, s.code, 'pair:' + s.device_id)}
                      >
                        确认配对
                      </BusyButton>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
          <label className="field" style={{ flex: '1 1 200px' }}>
            <div className="field-label">device_id（多台待确认时填写）</div>
            <input
              name="manual_device_id"
              value={manualDeviceId}
              onChange={(e) => setManualDeviceId(e.target.value)}
              placeholder="如 NOTE4C-3400FC…"
              autoComplete="off"
              spellCheck={false}
            />
          </label>
          <label className="field" style={{ flex: '0 0 150px' }}>
            <div className="field-label">6 位码（手动核对）</div>
            <input
              name="pair_code"
              value={pairCode}
              onChange={(e) => setPairCode(e.target.value.replace(/\D/g, ''))}
              placeholder="000000…"
              inputMode="numeric"
              autoComplete="one-time-code"
              spellCheck={false}
              maxLength={6}
              className="mono"
            />
          </label>
          <BusyButton className="btn secondary" busy={busy === 'pair:manual'} busyText="确认中…" onClick={confirmManual}>
            手动确认
          </BusyButton>
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <h2>设备列表</h2>
          <BusyButton
            className="btn secondary"
            busy={busy === 'devices'}
            busyText="刷新中…"
            onClick={() => refresh('devices', fetchDevices)}
          >
            刷新
          </BusyButton>
        </div>
        {devices.length === 0 ? (
          <p className="muted">暂无设备</p>
        ) : (
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">Device ID</th>
                  <th scope="col">板型</th>
                  <th scope="col">最近在线</th>
                  <th scope="col">首次上线</th>
                  <th scope="col">信任</th>
                  <th scope="col">电量</th>
                  <th scope="col">操作</th>
                </tr>
              </thead>
              <tbody>
                {devices.map((d) => (
                  <tr key={d.device_id}>
                    <td className="mono wrap-anywhere">{d.device_id}</td>
                    <td className="muted">{d.board_type || '—'}</td>
                    {/* The API sends epoch seconds; show the relative age with the
                        exact timestamp on hover. */}
                    <td title={formatTime(d.last_seen)}>{timeAgo(d.last_seen)}</td>
                    <td className="muted" title={formatTime(d.first_seen)}>{formatTime(d.first_seen)}</td>
                    <td><span className={`badge ${d.trust ? 'on' : 'off'}`}>{d.trust ? '已信任' : '未信任'}</span></td>
                    {(() => {
                      const p = d.power;
                      const pct = p && typeof p.battery_pct === 'number' ? p.battery_pct : null;
                      const chg = p && typeof p.battery_charge === 'number' ? p.battery_charge : null;
                      const chgLabel = chg === 2 ? '充电中' : chg === 3 ? '已充满'
                        : (chg === 4 || chg === 1) ? '放电中' : chg === 0 ? '状态未知' : null;
                      const chgColor = chg === 2 ? 'rgb(220,30,30)' : chg === 3 ? '#b8860b' : '#666';
                      return pct !== null ? (
                        <>
                          <td style={{ fontVariantNumeric: 'tabular-nums', fontSize: 12 }}>
                            <span title={`${p.battery_mv} mV`}>🔋 {pct}%</span>
                            {chgLabel && <span style={{ color: chgColor, marginLeft: 4, fontSize: 11 }}>{chgLabel}</span>}
                            <BatterySpark deviceId={d.device_id} />
                          </td>
                        </>
                      ) : <td style={{ fontSize: 12, color: '#aaa' }}>—</td>;
                    })()}
                    <td>
                      <BusyButton
                        className={d.trust ? 'btn danger' : 'btn'}
                        busy={busy === 'trust:' + d.device_id}
                        busyText="处理中…"
                        onClick={() => setTrust(d, !d.trust)}
                      >
                        {d.trust ? '吊销' : '信任'}
                      </BusyButton>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}

function BatterySpark({ deviceId }) {
  const [pts, setPts] = useState(null);
  const [hours, setHours] = useState(24);
  useEffect(() => {
    let alive = true;
    api.powerHistory(deviceId, hours)
      .then(d => alive && setPts(d.points ?? []))
      .catch(() => alive && setPts([]));
    return () => { alive = false; };
  }, [deviceId, hours]);
  const W = 320, H = 60, PAD = 2;
  const span = (pts && pts.length > 1) ? pts[pts.length - 1].ts - pts[0].ts : 1;
  const xy = p => [PAD + (p.ts - pts[0].ts) / span * (W - 2 * PAD),
                   H - PAD - (p.mv - 3300) / (4200 - 3300) * (H - 2 * PAD)];
  // Discharge (4/0/1) black, charging (2) palette red, full (3) palette yellow —
  // segments break on every charge-state change (spec: 充电段着色, 2026-09-22 修订).
  const chargeColor = c => (c === 2 ? 'rgb(220,30,30)' : c === 3 ? 'rgb(255,215,0)' : '#000');
  let segs = [], cur = [];
  (pts || []).forEach(p => {
    if (cur.length && cur[0].charge !== p.charge) { segs.push(cur); cur = []; }
    cur.push(p);
  });
  if (cur.length) segs.push(cur);
  return (
    <div style={{ marginTop: 6 }}>
      <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginBottom: 2 }}>
        <span style={{ fontSize: 11, color: 'var(--color-text-secondary, #666)' }}>电量曲线</span>
        {[24, 168, 720, 2160].map(h => (
          <button key={h} onClick={() => setHours(h)}
                  style={{ fontWeight: h === hours ? 'bold' : 'normal', fontSize: 11, padding: '1px 4px', background: 'none', border: '1px solid #ccc', borderRadius: 3, cursor: 'pointer', color: 'inherit' }}>
            {h === 24 ? '24h' : h === 168 ? '7d' : h === 720 ? '30d' : '90d'}
          </button>
        ))}
        <span style={{ fontSize: 10, color: '#888', marginLeft: 'auto', display: 'inline-flex', gap: 8 }} title="按充电状态着色的电压曲线">
          <span><span style={{ color: '#000' }}>▬</span> 放电</span>
          <span><span style={{ color: 'rgb(220,30,30)' }}>▬</span> 充电</span>
          <span><span style={{ color: 'rgb(255,215,0)' }}>▬</span> 充满</span>
        </span>
      </div>
      <svg width={W} height={H} role="img" aria-label="电池电压曲线" style={{ display: 'block' }}>
        {pts === null && <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="12" fill="#888">加载中…</text>}
        {pts !== null && pts.length < 2 &&
          <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="11" fill="#888">暂无数据（新固件生效后逐点累积）</text>}
        {segs.map((s, i) => (
          <polyline key={i} fill="none" stroke={chargeColor(s[0].charge)} strokeWidth="1.5"
                    points={s.map(p => xy(p).join(',')).join(' ')} />
        ))}
      </svg>
    </div>
  );
}
