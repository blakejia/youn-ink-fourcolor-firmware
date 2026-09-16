import React, { useEffect, useRef, useState } from 'react';
import { api } from './api.js';
import { Banner, BusyButton } from './ui.jsx';
import { formatBytes } from './format.js';
import {
  APP_PARTITION_SIZE, FLASH_PARAMS, OTADATA_OFFSET, OTADATA_SIZE, SLOTS, parseOtadata,
} from './flashTarget.js';
import { md5Hex } from './md5.js';

const STEP_IDLE = 'idle';
// 完整镜像备份区：bootloader(0x0~0x10000) 内含分区表与 otadata 头部，
// 先整 64 KiB 读出，写坏了还能用备份回滚。
const BOOT_REGION_OFFSET = 0x0;
const BOOT_REGION_SIZE = 0x10000;

const norm4 = (s) => (s || '').trim().toUpperCase();
const macLast4 = (mac) => (mac || '').replace(/:/g, '').slice(-4).toUpperCase();

/** 空 payload 静默跳过却报成功是最坏的失败：三条写入路径共用此闸门。 */
function assertNonEmpty(data, label) {
  if (!data || data.length === 0) {
    throw new Error(`${label}读到 0 字节，已拒绝写入（未写入任何字节）`);
  }
}

export default function FirmwareFlash({ onBusyChange } = {}) {
  const [supported] = useState(() => typeof navigator !== 'undefined' && !!navigator.serial);
  const [items, setItems] = useState([]);
  const [selected, setSelected] = useState('');
  const [err, setErr] = useState('');
  const [step, setStep] = useState(STEP_IDLE);
  const [progress, setProgress] = useState('');
  const [confirmText, setConfirmText] = useState('');
  const [target, setTarget] = useState(null);
  const [manualSlot, setManualSlot] = useState(null);
  const [deviceInfo, setDeviceInfo] = useState(null);
  const [backup, setBackup] = useState(true);
  const [advanced, setAdvanced] = useState(false);
  const [resetAck, setResetAck] = useState(false);
  const [fullAck, setFullAck] = useState(false);
  const [log, setLog] = useState('');
  const logRef = useRef(null);
  const fileRef = useRef(null);
  const fullFileRef = useRef(null);
  const portRef = useRef(null);
  const transportRef = useRef(null);
  const pctRef = useRef(-1);

  const busy = step !== STEP_IDLE;
  // 刷写中状态上抛：Serial.jsx 据此禁用页签切换 + useBlocker 拦路由跳转 +
  // registerUnsavedCheck 拦设备切换。关端口的守卫在父级，卸载即掐断写坏的路径被堵死。
  useEffect(() => { onBusyChange?.(busy); }, [busy, onBusyChange]);

  const say = (s) => setLog((prev) => `${prev}${s}\n`);

  const load = async () => {
    try { setItems(await api.firmwareList()); setErr(''); }
    catch (e) { setErr(e.message); }
  };
  useEffect(() => { load(); }, []);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [log]);

  // 刷写中关标签页/刷新 = 半写状态，必须拦住（SPA 内导航由 Serial.jsx 的 useBlocker 拦）。
  useEffect(() => {
    if (step === STEP_IDLE) return undefined;
    const h = (e) => { e.preventDefault(); e.returnValue = ''; };
    window.addEventListener('beforeunload', h);
    return () => window.removeEventListener('beforeunload', h);
  }, [step]);

  // 关端口的唯一出口：transport.disconnect() 内部就是
  // cancel（若 readable 被锁）→ 等解锁 → close，每步兜底；
  // 再补一次 port.close() 兜底已打开但 transport 没建好的情形。
  const closePort = async () => {
    const t = transportRef.current;
    const p = portRef.current;
    transportRef.current = null;
    portRef.current = null;
    try { await t?.disconnect?.(); } catch (e) { /* 已断或已关 */ }
    try { await p?.close?.(); } catch (e) { /* 已关或从未打开 */ }
  };
  const closePortRef = useRef(closePort);
  closePortRef.current = closePort;
  const stepRef = useRef(step);
  stepRef.current = step;

  // 卸载 cleanup：正常路径下 Serial.jsx 在 busy 时根本不让本组件卸载，
  // 此处是守卫失效时的最后一道 —— 不得静默，必须留下可查痕迹。
  // 注意：写入中途关端口会掐断刷写留下半写分区，所以 busy 时只告警、不主动掐断
  // esptool 自己的传输（端口随页面/组件销毁由浏览器回收），避免二次伤害。
  // 非 busy 时正常释放端口，避免泄漏。busy 判断用 stepRef（cleanup 闭包拿不到最新 state）。
  useEffect(() => () => {
    onBusyChange?.(false);
    if (stepRef.current !== STEP_IDLE) {
      // eslint-disable-next-line no-console
      console.error(
        `[FirmwareFlash] 组件在“${stepRef.current}”阶段被卸载：刷写可能未完成，`
        + '若已进入写入阶段设备可能处于半写状态 —— 不要断电，用备份回滚。'
        + '传输可能仍在后台完成；若设备未正常启动，用备份回滚。',
      );
      return;
    }
    const done = closePortRef.current();
    done.catch(() => {});
  }, [onBusyChange]);

  const downloadBytes = (bytes, filename) => {
    const blob = new Blob([bytes], { type: 'application/octet-stream' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  };

  const upload = async () => {
    const f = fileRef.current?.files?.[0];
    if (!f) { setErr('请选择 .bin 文件'); return; }
    setErr('');
    try {
      const item = await api.uploadFirmware(f);
      say(`已上传 ${item.name}（${formatBytes(item.size)}，sha256 ${item.sha256.slice(0, 12)}…）`);
      await load();
      setSelected(item.id);
    } catch (e) { setErr(e.message); }
  };

  // 建连并识别：懒加载 esptool-js → requestPort → main() → 读 MAC。
  // MAC 唯一可信入口是 esploader.chip.readMac(esploader)（ROM.readMac 抽象方法，
  // esp32s3 实现读 eFuse 寄存器，返回 "aa:bb:cc:dd:ee:ff" 形小写串；已对 0.6.1 实测核实）。
  const openSession = async (sayFn) => {
    const esptool = await import('esptool-js'); // 懒加载，不进首屏 bundle
    const port = await navigator.serial.requestPort();
    portRef.current = port;
    const terminal = {
      clean: () => {},
      writeLine: (s) => sayFn(s),
      write: (s) => setLog((prev) => prev + s),
    };
    const transport = new esptool.Transport(port, true);
    transportRef.current = transport;
    const esploader = new esptool.ESPLoader({ transport, baudrate: 115200, terminal, debugLogging: false });
    const chip = await esploader.main();
    sayFn(`芯片：${chip}`);
    if (!/ESP32-S3/i.test(chip)) {
      throw new Error(`这不是 ESP32-S3（读到 ${chip}），已中止，未写入任何字节`);
    }
    const mac = (await esploader.chip.readMac(esploader)).toUpperCase();
    sayFn(`MAC：${mac}`);
    return { esploader, chip, mac };
  };

  const checkConfirm = (mac) => {
    const got = norm4(confirmText);
    const want = macLast4(mac);
    if (!/^[0-9A-F]{4}$/.test(got)) {
      throw new Error('请先输入设备 MAC 末四位（4 位十六进制），已中止，未写入任何字节');
    }
    if (got !== want) {
      throw new Error(`确认串 ${got} 与本机设备 MAC ${mac}（末四位 ${want}）不符，已中止，未写入任何字节`);
    }
  };

  const onProgress = (label) => (i, written, total) => {
    const pct = total ? Math.floor((written / total) * 100) : 0;
    if (pct !== pctRef.current) {
      pctRef.current = pct;
      setProgress(`${label} ${pct}%（${formatBytes(written)} / ${formatBytes(total)}）`);
    }
    if (written >= total) say(`${label}完成 ${formatBytes(total)}`);
  };

  // readFlash(addr, size, onPacketReceived(packet, received, total))：备份读 4032 KiB
  // 要跑很久，必须有进度，否则用户以为卡死而拔线。
  const readRaw = async (esploader, addr, size, label) => {
    pctRef.current = -1;
    const onData = label ? onProgress(label) : () => {};
    const raw = await esploader.readFlash(addr, size, (pkt, got, total) => onData(0, got, total));
    const out = raw instanceof Uint8Array ? raw : new Uint8Array(raw);
    assertNonEmpty(out, `${label || '读 flash'}（0x${addr.toString(16)}）`);
    return out;
  };

  const abortMessage = (e, enteredWrite, rollbackHint) => (enteredWrite
    ? `刷写中止：${e.message}；已经进入写入阶段，设备可能处于半写状态 —— 不要断电，${rollbackHint}。`
    : `刷写中止：${e.message}（尚未进入写入阶段，未写入任何字节）。`);

  const flash = async () => {
    const item = items.find((i) => i.id === selected);
    if (!item) { setErr('请先选择固件'); return; }
    if (item.image_ok === false) {
      setErr('该固件服务端校验未通过（image_ok=false：首字节不是 0xE9 或大小异常），已拒绝写入，未写入任何字节');
      return;
    }
    if (!/^[0-9A-Fa-f]{4}$/.test(norm4(confirmText))) {
      setErr('请先输入设备 MAC 末四位（4 位十六进制），对照设备标签填写');
      return;
    }
    if (!backup) {
      const ok = window.confirm('已关闭“刷前备份”。一旦写坏将无法回滚。仍要继续吗？');
      if (!ok) return;
    }
    setErr('');
    setLog('');
    setProgress('');
    pctRef.current = -1;
    setTarget(null);
    let enteredWrite = false;
    let rollback = '用备份按相同步骤写回原槽位回滚';
    try {
      setStep('读固件');
      const bytes = await api.firmwareBytes(item.id);
      const data = new Uint8Array(bytes);
      assertNonEmpty(data, `固件 ${item.name}`);
      if (data.length > APP_PARTITION_SIZE) {
        throw new Error(
          `镜像 ${formatBytes(data.length)} 超过应用分区上限 ${formatBytes(APP_PARTITION_SIZE)}（4032 KiB），`
          + '不能走 OTA 槽路径 —— 请用下方高级「完整镜像」路径（写 0x0）。已中止，未写入任何字节',
        );
      }
      say(`固件 ${item.name} ${formatBytes(data.length)} sha256 ${item.sha256.slice(0, 12)}…`);

      setStep('打开串口');
      const { esploader, chip, mac } = await openSession(say);
      setDeviceInfo({ chip, mac });
      checkConfirm(mac);

      setStep('读 otadata 判定目标槽');
      const otaBytes = await readRaw(esploader, OTADATA_OFFSET, OTADATA_SIZE);
      const auto = parseOtadata(otaBytes);
      const t = manualSlot || auto;
      setTarget(t);
      say(`目标槽：${t.name} @0x${t.offset.toString(16)}（${t.evidence}）${manualSlot ? '（手动覆盖）' : ''}`);

      if (backup) {
        setStep('备份当前固件');
        const bk = await readRaw(esploader, t.offset, APP_PARTITION_SIZE, '备份读取');
        downloadBytes(bk, `backup-${mac.replace(/:/g, '')}-${t.name}-${Date.now()}.bin`);
        say(`已下载备份 ${formatBytes(bk.length)}：刷坏了可用它按相同步骤写回 0x${t.offset.toString(16)} 回滚`);
      }
      rollback = `用备份写回 0x${t.offset.toString(16)} 回滚`;

      setStep('写入');
      enteredWrite = true;
      await esploader.writeFlash({
        fileArray: [{ data, address: t.offset }],
        flashMode: FLASH_PARAMS.flashMode,
        flashFreq: FLASH_PARAMS.flashFreq,
        flashSize: FLASH_PARAMS.flashSize,
        eraseAll: false,
        compress: true,
        reportProgress: onProgress('写入（压缩后字节）'),
        calculateMD5Hash: (image) => md5Hex(image),
      });
      // writeFlash 返回 = 设备端 flashMd5sum 与本地 MD5 比对一致（不一致时抛
      // "MD5 of file does not match data in flash!"），成功消息只在此后出现。
      say('写入 MD5 校验通过（设备端 flashMd5sum 与本地一致）。');

      setStep('复位');
      await esploader.after('hard_reset');
      setStep(STEP_IDLE);
      setProgress('');
      await closePort();
      say('刷写结束：设备已复位，端口已释放。若要验证，去「串口监视」页签重新打开串口看开机日志。');
    } catch (e) {
      setErr(abortMessage(e, enteredWrite, rollback));
      setStep(STEP_IDLE);
      setProgress('');
      await closePort();
    }
  };

  // 高级：重置启动选择回 ota_0。先备份 otadata 再全 FF 写回。
  const resetBoot = async () => {
    if (!resetAck) { setErr('请先勾选“确认重置启动选择”'); return; }
    if (!/^[0-9A-Fa-f]{4}$/.test(norm4(confirmText))) {
      setErr('请先输入设备 MAC 末四位（4 位十六进制）');
      return;
    }
    const ok = window.confirm('将清空 otadata（启动选择回落到 ota_0）。仍要继续吗？');
    if (!ok) return;
    setErr('');
    setProgress('');
    pctRef.current = -1;
    let enteredWrite = false;
    try {
      setStep('打开串口');
      const { mac, esploader } = await openSession(say);
      checkConfirm(mac);
      setStep('备份 otadata');
      const ota = await readRaw(esploader, OTADATA_OFFSET, OTADATA_SIZE, 'otadata 备份读取');
      downloadBytes(ota, `otadata-backup-${mac.replace(/:/g, '')}-${Date.now()}.bin`);
      say(`已下载 otadata 备份 ${formatBytes(ota.length)}`);
      setStep('清空 otadata');
      enteredWrite = true;
      await esploader.writeFlash({
        fileArray: [{ data: new Uint8Array(OTADATA_SIZE).fill(0xff), address: OTADATA_OFFSET }],
        flashMode: FLASH_PARAMS.flashMode,
        flashFreq: FLASH_PARAMS.flashFreq,
        flashSize: FLASH_PARAMS.flashSize,
        eraseAll: false,
        compress: true,
        reportProgress: onProgress('清空 otadata（压缩后字节）'),
        calculateMD5Hash: (image) => md5Hex(image),
      });
      setStep('复位');
      await esploader.after('hard_reset');
      setStep(STEP_IDLE);
      setProgress('');
      await closePort();
      say('已重置：启动选择回落到 ota_0，端口已释放。');
    } catch (e) {
      setErr(abortMessage(e, enteredWrite, '用刚下载的 otadata 备份写回 0xd000 回滚'));
      setStep(STEP_IDLE);
      setProgress('');
      await closePort();
    }
  };

  // 高级：完整镜像（写 0x0，含 bootloader/分区表）。从本地文件读，
  // 不走服务端上传（服务端上传上限 4032 KiB 装不下整个镜像）。
  const flashFull = async () => {
    const f = fullFileRef.current?.files?.[0];
    if (!f) { setErr('请先选择本地完整镜像 .bin'); return; }
    if (!fullAck) { setErr('请先勾选“确认完整镜像风险”'); return; }
    if (!/^[0-9A-Fa-f]{4}$/.test(norm4(confirmText))) {
      setErr('请先输入设备 MAC 末四位（4 位十六进制）');
      return;
    }
    const ok = window.confirm(
      `即将把本地文件「${f.name}」（${formatBytes(f.size)}）写入目标地址 0x0（覆盖 bootloader 与分区表），`
      + '写坏即变砖（需备份回滚）。应用分区镜像（小文件）绝不能走这条路径，只能走上面的 OTA 槽刷写。仍要继续吗？',
    );
    if (!ok) return;
    setErr('');
    setProgress('');
    pctRef.current = -1;
    let enteredWrite = false;
    try {
      setStep('读本地镜像');
      const data = new Uint8Array(await f.arrayBuffer());
      assertNonEmpty(data, `本地文件 ${f.name}`);
      say(`本地镜像 ${f.name} ${formatBytes(data.length)} → 目标地址 0x0`);
      if (data[0] !== 0xe9) {
        throw new Error('该文件首字节不是 0xE9，不像 ESP 镜像，已中止，未写入任何字节');
      }
      if (data.length <= APP_PARTITION_SIZE) {
        const ok2 = window.confirm(
          `警告：「${f.name}」只有 ${formatBytes(data.length)}，不大于应用分区 ${formatBytes(APP_PARTITION_SIZE)}，`
          + '看起来像应用分区镜像而非完整合并镜像 —— 写入 0x0 会毁掉 bootloader。确定它真的是写 0x0 的完整镜像吗？',
        );
        if (!ok2) { setStep(STEP_IDLE); setProgress(''); return; }
      }
      setStep('打开串口');
      const { mac, esploader } = await openSession(say);
      checkConfirm(mac);
      setStep('备份 bootloader 区');
      const bl = await readRaw(esploader, BOOT_REGION_OFFSET, BOOT_REGION_SIZE, 'boot 区备份读取');
      downloadBytes(bl, `bl-pt-otadata-backup-${mac.replace(/:/g, '')}-${Date.now()}.bin`);
      say(`已下载 bl-pt-otadata 备份 ${formatBytes(bl.length)}`);
      setStep('写入完整镜像');
      enteredWrite = true;
      await esploader.writeFlash({
        fileArray: [{ data, address: BOOT_REGION_OFFSET }],
        flashMode: FLASH_PARAMS.flashMode,
        flashFreq: FLASH_PARAMS.flashFreq,
        flashSize: FLASH_PARAMS.flashSize,
        eraseAll: false,
        compress: true,
        reportProgress: onProgress('写入完整镜像（压缩后字节）'),
        calculateMD5Hash: (image) => md5Hex(image),
      });
      say('写入 MD5 校验通过（设备端 flashMd5sum 与本地一致）。');
      setStep('复位');
      await esploader.after('hard_reset');
      setStep(STEP_IDLE);
      setProgress('');
      await closePort();
      say('完整镜像写入结束：设备已复位，端口已释放。去「串口监视」看开机日志确认。');
    } catch (e) {
      setErr(abortMessage(e, enteredWrite, '用 bl-pt-otadata 备份写回 0x0 回滚，应用分区备份另需写回原槽位'));
      setStep(STEP_IDLE);
      setProgress('');
      await closePort();
    }
  };

  if (!supported) {
    return (
      <Banner>
        这个浏览器没有 Web Serial，无法刷写。请用桌面版 Chrome / Edge 89+ 或
        Firefox 151+，并确保页面是 HTTPS 或 localhost。
      </Banner>
    );
  }

  return (
    <div>
      <Banner>{err}</Banner>
      <p className="muted">
        默认只写<strong>活动 OTA 槽的应用分区</strong>（bootloader / 分区表 / NVS 不碰），
        刷前自动读出当前固件并下载为备份。写入前需输入设备 MAC 末四位确认。
        刷写进行中时页签切换与离开本页会被拦下（避免半写变砖）。
      </p>

      <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
        <label className="field" style={{ flex: '1 1 220px' }}>
          <div className="field-label">服务器上的固件</div>
          <select value={selected} onChange={(e) => setSelected(e.target.value)} disabled={busy}>
            <option value="">— 选择 —</option>
            {items.map((i) => (
              <option key={i.id} value={i.id}>
                {i.source === 'build' ? '[构建产物] ' : '[上传] '}{i.name} · {formatBytes(i.size)}
                {i.image_ok === false ? ' · 校验未通过' : ''}
              </option>
            ))}
          </select>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">清单</div>
          <button type="button" className="btn secondary" onClick={load} disabled={busy}>刷新清单</button>
        </label>
      </div>

      <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
        <label className="field" style={{ flex: '1 1 220px' }}>
          <div className="field-label">或上传一个 .bin</div>
          <input type="file" accept=".bin" ref={fileRef} disabled={busy} />
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">上传</div>
          <BusyButton busy={busy} onClick={upload}>上传</BusyButton>
        </label>
      </div>

      {deviceInfo && (
        <p className="muted">
          上次识别：{deviceInfo.chip} · MAC {deviceInfo.mac}
        </p>
      )}
      {target && (
        <p className="muted">
          目标：<strong>{target.name} @0x{target.offset.toString(16)}</strong>
          {' '}（{target.evidence}）
        </p>
      )}

      <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
        <label className="field" style={{ flex: '0 0 180px' }}>
          <div className="field-label">输入设备 MAC 末四位以确认</div>
          <input
            value={confirmText}
            onChange={(e) => setConfirmText(e.target.value)}
            disabled={busy}
            placeholder="例：3400"
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">备份</div>
          <span>
            <input
              type="checkbox"
              checked={backup}
              onChange={(e) => setBackup(e.target.checked)}
              disabled={busy}
            />
            刷前备份当前固件（建议保持开启）
          </span>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">刷写</div>
          <BusyButton busy={busy} busyText={busy ? `${step}…` : ''} onClick={flash}>
            开始刷写
          </BusyButton>
        </label>
      </div>
      {progress && <p className="muted">{progress}</p>}

      <label className="field" style={{ marginTop: 12 }}>
        <div className="field-label">高级</div>
        <span>
          <input type="checkbox" checked={advanced} onChange={(e) => setAdvanced(e.target.checked)} />
          展开高级选项（手动覆盖目标槽 / 重置启动选择 / 完整镜像）
        </span>
      </label>
      {advanced && (
        <div>
          <div className="row" style={{ marginTop: 8, alignItems: 'flex-end' }}>
            <label className="field" style={{ flex: '1 1 220px' }}>
              <div className="field-label">手动覆盖目标槽</div>
              <span>
                {SLOTS.map((s) => (
                  <button
                    key={s.name}
                    type="button"
                    className={manualSlot?.name === s.name ? 'btn' : 'btn secondary'}
                    disabled={busy}
                    style={{ marginRight: 8 }}
                    onClick={() => setManualSlot(
                      manualSlot?.name === s.name
                        ? null
                        : { slotIndex: SLOTS.indexOf(s), ...s, seq: null, evidence: '手动覆盖', verified: false },
                    )}
                  >
                    {s.name} @0x{s.offset.toString(16)}
                  </button>
                ))}
              </span>
            </label>
          </div>

          <div className="row" style={{ marginTop: 8, alignItems: 'flex-end' }}>
            <label className="field" style={{ flex: '1 1 220px' }}>
              <div className="field-label">重置启动选择回 ota_0（先备份 otadata 再清空）</div>
              <span>
                <input type="checkbox" checked={resetAck} onChange={(e) => setResetAck(e.target.checked)} disabled={busy} />
                确认重置启动选择
              </span>
            </label>
            <label className="field" style={{ flex: '0 0 auto' }}>
              <div className="field-label">执行</div>
              <BusyButton busy={busy} busyText={busy ? `${step}…` : ''} onClick={resetBoot}>
                重置启动选择
              </BusyButton>
            </label>
          </div>

          <div className="row" style={{ marginTop: 8, alignItems: 'flex-end' }}>
            <label className="field" style={{ flex: '1 1 220px' }}>
              <div className="field-label">完整镜像（写 0x0，含 bootloader/分区表，从本地文件读）</div>
              <input type="file" accept=".bin" ref={fullFileRef} disabled={busy} />
            </label>
            <label className="field" style={{ flex: '0 0 auto' }}>
              <div className="field-label">风险确认</div>
              <span>
                <input type="checkbox" checked={fullAck} onChange={(e) => setFullAck(e.target.checked)} disabled={busy} />
                确认写入 0x0（覆盖 bootloader/分区表）
              </span>
            </label>
            <label className="field" style={{ flex: '0 0 auto' }}>
              <div className="field-label">执行</div>
              <BusyButton busy={busy} busyText={busy ? `${step}…` : ''} onClick={flashFull}>
                写入完整镜像
              </BusyButton>
            </label>
          </div>
        </div>
      )}

      <pre ref={logRef} className="mono" style={{ maxHeight: 320, overflow: 'auto' }}>{log}</pre>
      <p className="muted">写入完成前不要关页面、不要拔线。</p>
    </div>
  );
}
// esptool-js 要求调用方提供 MD5 实现（它自己不打包 crypto）；
// md5Hex 来自 ./md5.js。ESM 下只能用 import，不能用 require。
