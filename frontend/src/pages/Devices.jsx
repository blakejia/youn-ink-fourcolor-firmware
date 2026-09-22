import React, { useEffect, useRef, useState } from 'react';
import { api } from '../api.js';
import { Banner, BusyButton } from '../ui.jsx';
import { formatCountdown, formatTime, timeAgo } from '../format.js';

// Charge-state vocabulary shared by the cell badge, the curve colors, the
// legend and the hover readout — one source so they can never disagree.
const chargeLabel = c => (c === 2 ? '充电中' : c === 3 ? '已充满'
  : (c === 4 || c === 1) ? '放电中' : c === 0 ? '状态未知' : null);
const chargeColor = c => (c === 2 ? 'rgb(220,30,30)' : c === 3 ? 'rgb(255,215,0)' : '#000');
const chargeBadgeColor = c => (c === 2 ? 'rgb(220,30,30)' : c === 3 ? '#b8860b' : '#666');

// Consecutive points sharing one charge state form one polyline run.
function splitSegs(pts) {
  let segs = [], cur = [];
  (pts || []).forEach(p => {
    if (cur.length && cur[0].charge !== p.charge) { segs.push(cur); cur = []; }
    cur.push(p);
  });
  if (cur.length) segs.push(cur);
  return segs;
}

function makeXY(pts, W, H, PAD) {
  const span = (pts && pts.length > 1) ? pts[pts.length - 1].ts - pts[0].ts : 1;
  return p => [PAD + (p.ts - pts[0].ts) / span * (W - 2 * PAD),
               H - PAD - (p.mv - 3300) / (4200 - 3300) * (H - 2 * PAD)];
}

function Segs({ pts, xy, strokeW = 1.5 }) {
  return splitSegs(pts).map((s, i) => (
    <polyline key={i} fill="none" stroke={chargeColor(s[0].charge)} strokeWidth={strokeW}
              points={s.map(p => xy(p).join(',')).join(' ')} />
  ));
}

function ChargeLegend({ fontSize = 10 }) {
  return (
    <span style={{ fontSize, color: '#666', display: 'inline-flex', gap: 8 }} title="按充电状态着色的电压曲线">
      <span><span style={{ color: '#000' }}>▬</span> 放电</span>
      <span><span style={{ color: 'rgb(220,30,30)' }}>▬</span> 充电</span>
      <span><span style={{ color: 'rgb(255,215,0)' }}>▬</span> 充满</span>
    </span>
  );
}

export default function Devices() {
  const [devices, setDevices] = useState([]);
  const [err, setErr] = useState('');
  const [pending, setPending] = useState([]); // [{device_id, code, expires_in}]
  const [pairCode, setPairCode] = useState('');
  const [manualDeviceId, setManualDeviceId] = useState('');
  // Key of the action currently in flight, so the right button reports it.
  const [busy, setBusy] = useState('');
  // Battery detail modal: null = closed, else the device row to inspect.
  const [detailDevice, setDetailDevice] = useState(null);

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
                      const chgLabel = chargeLabel(chg);
                      return pct !== null ? (
                        <td
                          className="bat-cell"
                          style={{ fontVariantNumeric: 'tabular-nums', fontSize: 12, cursor: 'pointer' }}
                          title="点击查看多天电量详情"
                          onClick={() => setDetailDevice(d)}
                          onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); setDetailDevice(d); } }}
                          role="button"
                          tabIndex={0}
                        >
                          <span title={`${p.battery_mv} mV`}>🔋 {pct}%</span>
                          {chgLabel && <span style={{ color: chargeBadgeColor(chg), marginLeft: 4, fontSize: 11 }}>{chgLabel}</span>}
                          <BatterySpark deviceId={d.device_id} />
                        </td>
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

      {detailDevice && (
        <BatteryDetail device={detailDevice} onClose={() => setDetailDevice(null)} />
      )}
    </div>
  );
}

// Inline spark in the table cell: fixed 24 h window, no window switch — the
// cell is a summary; clicking it opens BatteryDetail for the multi-day view.
function BatterySpark({ deviceId }) {
  const [pts, setPts] = useState(null);
  useEffect(() => {
    let alive = true;
    api.powerHistory(deviceId, 24)
      .then(d => alive && setPts(d.points ?? []))
      .catch(() => alive && setPts([]));
    return () => { alive = false; };
  }, [deviceId]);
  const W = 320, H = 60, PAD = 2;
  const xy = pts && pts.length ? makeXY(pts, W, H, PAD) : null;
  return (
    <div style={{ marginTop: 6 }}>
      <div style={{ display: 'flex', gap: 6, alignItems: 'center', marginBottom: 2 }}>
        <span style={{ fontSize: 11, color: 'var(--color-text-secondary, #666)' }}>电量曲线 · 24h</span>
        <span style={{ marginLeft: 'auto' }}><ChargeLegend /></span>
      </div>
      <svg width={W} height={H} role="img" aria-label="电池电压曲线（24 小时）" style={{ display: 'block' }}>
        {pts === null && <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="12" fill="#666">加载中…</text>}
        {pts !== null && pts.length < 2 &&
          <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="11" fill="#666">暂无数据（新固件生效后逐点累积）</text>}
        {xy && <Segs pts={pts} xy={xy} />}
      </svg>
    </div>
  );
}

// Modal detail view: big SVG, 24h/7d/30d/90d window switch, hover readout.
// Follows the ConfirmDialog interaction contract: backdrop click and Escape
// close it; the × button is the explicit close. The readout is driven by
// pointer events (mouse + touch + pen), and keyboard users scrub points with
// ←/→/Home/End on the focusable chart — hover is never the only path.
function BatteryDetail({ device, onClose }) {
  const [hours, setHours] = useState(24);
  const [pts, setPts] = useState(null);
  const [hoverIdx, setHoverIdx] = useState(null);
  const dialogRef = useRef(null);
  const W = 640, H = 240, PAD = 12;
  useEffect(() => {
    let alive = true;
    setPts(null);
    setHoverIdx(null);
    api.powerHistory(device.device_id, hours)
      .then(d => alive && setPts(d.points ?? []))
      .catch(() => alive && setPts([]));
    return () => { alive = false; };
  }, [device.device_id, hours]);
  useEffect(() => {
    const onKey = e => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
  useEffect(() => { dialogRef.current?.focus(); }, []);
  const p = device.power;
  const pct = p && typeof p.battery_pct === 'number' ? p.battery_pct : null;
  const chg = p && typeof p.battery_charge === 'number' ? p.battery_charge : null;
  const xy = pts && pts.length ? makeXY(pts, W, H, PAD) : null;
  const pick = (clientX, target) => {
    if (!xy) return;
    const rect = target.getBoundingClientRect();
    const fx = (clientX - rect.left) / rect.width * W;
    let bestIdx = 0, bd = Infinity;
    pts.forEach((q, i) => {
      const dx = Math.abs(xy(q)[0] - fx);
      if (dx < bd) { bd = dx; bestIdx = i; }
    });
    setHoverIdx(bestIdx);
  };
  const onSvgKey = e => {
    if (!pts || !pts.length) return;
    const last = pts.length - 1;
    const cur = hoverIdx == null ? last : hoverIdx;
    if (e.key === 'ArrowLeft') { e.preventDefault(); setHoverIdx(Math.max(0, cur - 1)); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); setHoverIdx(Math.min(last, cur + 1)); }
    else if (e.key === 'Home') { e.preventDefault(); setHoverIdx(0); }
    else if (e.key === 'End') { e.preventDefault(); setHoverIdx(last); }
  };
  const hover = hoverIdx != null && pts && pts[hoverIdx] ? pts[hoverIdx] : null;
  return (
    <div className="modal-backdrop" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="modal" role="dialog" aria-modal="true" tabIndex={-1} ref={dialogRef}
           aria-label={`${device.device_id} 电量详情`} style={{ width: 'max-content', maxWidth: '95vw' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <h2 className="modal-title" style={{ margin: 0 }}>电量详情 · {device.device_id}</h2>
          {pct !== null && <span style={{ fontSize: 13 }}>🔋 {pct}%</span>}
          {chargeLabel(chg) && <span style={{ fontSize: 13, color: chargeBadgeColor(chg) }}>{chargeLabel(chg)}</span>}
          <button type="button" className="spark-close" onClick={onClose} aria-label="关闭">×</button>
        </div>
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', margin: '10px 0 6px' }}>
          {[24, 168, 720, 2160].map(h => (
            <button key={h} className="spark-win" aria-pressed={h === hours} onClick={() => setHours(h)}>
              {h === 24 ? '24h' : h === 168 ? '7d' : h === 720 ? '30d' : '90d'}
            </button>
          ))}
          <span style={{ marginLeft: 'auto' }}><ChargeLegend fontSize={11} /></span>
        </div>
        <div className="spark-readout" role="status" aria-live="polite">
          {hover
            ? `${formatTime(hover.ts)} · ${hover.mv} mV · ${hover.pct}% · ${chargeLabel(hover.charge) ?? '—'}`
            : '悬停、拖动或用 ←/→ 键查看各点读数'}
        </div>
        <svg width={W} height={H} role="img" aria-label={`${device.device_id} 电压曲线，可用方向键查看各点`}
             tabIndex={0} onKeyDown={onSvgKey}
             onPointerMove={(e) => pick(e.clientX, e.currentTarget)}
             onPointerDown={(e) => pick(e.clientX, e.currentTarget)}
             onPointerLeave={() => setHoverIdx(null)}
             style={{ display: 'block', background: 'var(--color-surface)', border: '1px solid var(--color-border)',
                      cursor: 'crosshair', touchAction: 'pan-y' }}>
          {pts === null && <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="14" fill="#666">加载中…</text>}
          {pts !== null && pts.length < 2 &&
            <text x={W / 2} y={H / 2} textAnchor="middle" fontSize="13" fill="#666">暂无数据（新固件生效后逐点累积）</text>}
          {xy && <Segs pts={pts} xy={xy} strokeW={2} />}
          {xy && hover && (
            <g>
              <line x1={xy(hover)[0]} y1={PAD} x2={xy(hover)[0]} y2={H - PAD}
                    stroke="#999" strokeWidth={1} strokeDasharray="3 3" />
              <circle cx={xy(hover)[0]} cy={xy(hover)[1]} r={4}
                      fill={chargeColor(hover.charge)} stroke="var(--color-surface)" strokeWidth={1.5} />
            </g>
          )}
        </svg>
      </div>
    </div>
  );
}
