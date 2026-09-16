import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Banner, BusyButton } from './ui.jsx';
import { formatTime } from './format.js';
import { LineDecoder, RingBuffer, RING_LINE_CAP } from './serialLog.js';

const BAUD_CHOICES = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];

const pad2 = (n) => String(n).padStart(2, '0');
// 本地时间戳 [HH:MM:SS.mmm]：仓库约定是本地时间（format.js "Absolute local time"），
// 不能用 toISOString（UTC，看着像本地时间实差 8 小时）。
const stampOf = (d) => `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}.${String(d.getMilliseconds()).padStart(3, '0')}`;

export default function SerialConsole() {
  const [supported] = useState(() => typeof navigator !== 'undefined' && !!navigator.serial);
  const [baud, setBaud] = useState(115200);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');
  const [paused, setPaused] = useState(false);
  const [filter, setFilter] = useState('');
  const [stamp, setStamp] = useState(false);
  const [text, setText] = useState('');
  const [lineCount, setLineCount] = useState(0);

  const portRef = useRef(null);
  const readerRef = useRef(null);
  const bufferRef = useRef(new RingBuffer());
  const decoderRef = useRef(new LineDecoder());
  // 世代计数：connect/disconnect/掉线每次递增。读循环只收敛自己那一代，
  // 手动断开后才醒来的旧循环按 guard 跳过，不覆盖更准的提示。
  const epochRef = useRef(0);
  const pausedRef = useRef(false);
  const stampRef = useRef(false);
  const filterRef = useRef('');
  const rafRef = useRef(0);
  const preRef = useRef(null);
  const stickRef = useRef(true);

  useEffect(() => { pausedRef.current = paused; }, [paused]);
  useEffect(() => { stampRef.current = stamp; }, [stamp]);
  useEffect(() => { filterRef.current = filter; }, [filter]);

  // 批量渲染：每帧最多重画一次，避免高频启动日志把 DOM 打爆
  const scheduleRender = () => {
    if (rafRef.current) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = 0;
      const buf = bufferRef.current;
      // 用 ref 读过滤词：读循环闭包是 connect 那一刻的，state 会过期
      const kw = filterRef.current;
      const lines = kw ? buf.lines.filter((l) => l.includes(kw)) : buf.lines;
      setText(lines.slice(-RING_LINE_CAP).join('\n'));
      setLineCount(buf.lines.length);
    });
  };

  // 自动滚动必须在 DOM 提交后量：rAF 回调里 setText 紧接着读 scrollHeight
  // 拿到的是上一批的高度（React 18 createRoot 下提交在回调返回后），滚动永远落后一批。
  useLayoutEffect(() => {
    const el = preRef.current;
    if (el && stickRef.current) el.scrollTop = el.scrollHeight;
  }, [text]);
  // 关端口的唯一出口：先 await cancel，再解开 readable 的锁，最后 close，每步兜底。
  // ⚠️ 顺序不能错：cancel 没轮完（读循环 finally 里的 releaseLock 还没执行）就 close，
  // readable 仍处于 locked，Chrome/Blink 会拒绝关闭（AbortClose）——端口留在打开态，
  // 设备不恢复深睡，下一次 open() 报 already open。
  const closePort = async () => {
    const r = readerRef.current;
    const p = portRef.current;
    try { await r?.cancel(); } catch (e) { /* 已经断了 */ }
    try { r?.releaseLock(); } catch (e) { /* 锁已解开 */ }
    try { await p?.close(); } catch (e) { /* 可能已关闭或已断线 */ }
  };


  // 读循环异常/流结束后的统一收敛：吐半行 → 关端口 → 状态复位。
  // 不收敛的话界面会卡在"显示断开（看着还开着）却再无数据"，只能靠用户手点断开。
  const settleClosed = async (port, reader, message) => {
    const r = reader || readerRef.current;
    // flush 之后此实例不再复用 —— 直接换新（旧实例 TextDecoder 可能残留多字节暂存字节）。
    try {
      const tail = decoderRef.current.flush();
      decoderRef.current = new LineDecoder();
      if (tail.length) {
        bufferRef.current.push(tail);
        if (!pausedRef.current) scheduleRender();
      }
    } catch (e) { /* 无内容可刷 */ }
    // 先 cancel + 解锁再 close：此时读循环的 reader 还拿着锁（finally 还没跑），
    // 不解锁就 close 会被 Blink 拒绝（AbortClose）。
    await closePort();
    if (portRef.current === port) portRef.current = null;
    if (r && readerRef.current === r) readerRef.current = null;
    setOpen(false);
    if (message) setErr(message);
  };

  // 单读者：同一时刻只有一个 reader。
  const readLoop = async (port, myEpoch) => {
    const live = () => portRef.current === port && epochRef.current === myEpoch;
    let reader = null;
    try {
      // getReader 必须在 try 里：readable 为 null/已被锁时会抛，否则是无人处理的拒约。
      reader = port.readable.getReader();
      readerRef.current = reader;
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        const lines = decoderRef.current.push(value);
        if (lines.length) {
          if (stampRef.current) {
            const t = stampOf(new Date());
            bufferRef.current.push(lines.map((l) => `[${t}] ${l}`));
          } else {
            bufferRef.current.push(lines);
          }
          if (!pausedRef.current) scheduleRender();
        }
      }
    } catch (e) {
      // 手动断开/掉线已经清过 portRef 并写过更准的提示，不覆盖；旧世代直接跳过。
      if (live()) await settleClosed(port, reader, `读取中断：${e.message}`);
      return;
    } finally {
      try { reader?.releaseLock(); } catch (e) { /* 已释放 */ }
      if (reader && readerRef.current === reader) readerRef.current = null;
    }
    // read() 回 done（设备关流/掉线）同样收敛，否则卡在"已打开但无 reader"。
    if (live()) {
      await settleClosed(port, reader, '读取结束（设备关闭了数据流）。不会自动重连 —— 点「打开串口并复位设备」重连。');
    }
  };

  const connect = async () => {
    if (busy) return;
    setBusy(true);
    setErr('');
    try {
      const port = await navigator.serial.requestPort();
      await port.open({ baudRate: baud });
      const myEpoch = ++epochRef.current;
      portRef.current = port;
      setOpen(true);
      // 打开会复位设备：清空旧内容，从头开始看
      bufferRef.current.clear();
      decoderRef.current = new LineDecoder();
      scheduleRender();
      // readLoop 内部已收敛一切可预见错误；这里的 catch 只兜 try 之外的意外拒约。
      readLoop(port, myEpoch).catch((e) => {
        if (portRef.current === port && epochRef.current === myEpoch) {
          settleClosed(port, null, `读取中断：${e?.message || e}`);
        }
      });
    } catch (e) {
      // 用户取消端口选择是 NotFoundError：别报成"被别的程序占用"，误导人。
      if (e?.name === 'NotFoundError') setErr('未选择串口（已取消）。');
      else setErr(`打开失败：${e.message}（本机另一个程序可能占着这个端口）`);
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    if (busy) return;
    setBusy(true);
    try {
      epochRef.current++;
      // cancel → 解锁 → close 全在 closePort 里按顺序 await，每步有兜底。
      await closePort();
      // 解码器会把以孤立 \r 结尾的半行挂起：停止时必须 flush 出来，否则丢一行。
      // flush 之后此实例不再复用 —— 直接换新。
      try {
        const tail = decoderRef.current.flush();
        decoderRef.current = new LineDecoder();
        if (tail.length) {
          bufferRef.current.push(tail);
          if (!pausedRef.current) scheduleRender();
        }
      } catch (e) { /* 无内容可刷 */ }
      portRef.current = null;
      readerRef.current = null;
      setOpen(false);
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    // serial 先抓到局部变量：挂载后若有人把 navigator.serial 删掉
    //（降级测试即如此），cleanup 里再读 navigator.serial 会崩。
    const serial = typeof navigator !== 'undefined' ? navigator.serial : undefined;
    if (!supported || !serial) return undefined;
    const onDisconnect = async (e) => {
      if (portRef.current && e.target === portRef.current) {
        // 先递增世代：随后报错醒来的旧读循环按 guard 跳过，不覆盖下面的提示。
        epochRef.current++;
        // 设备掉线同样先把挂起的半行刷出来（此实例不再复用，直接换新）。
        try {
          const tail = decoderRef.current.flush();
          decoderRef.current = new LineDecoder();
          if (tail.length) {
            bufferRef.current.push(tail);
            if (!pausedRef.current) scheduleRender();
          }
        } catch (err2) { /* 无内容可刷 */ }
        // 浏览器在设备消失时未必自动关闭端口：补一次 cancel → 解锁 → close
        //（顺序见 closePort，错了会被 Blink 拒绝；端口已关闭时 close() 抛 InvalidStateError，吞掉即可）。
        await closePort();
        portRef.current = null;
        readerRef.current = null;
        setErr('设备已断开（USB 掉线或设备进入深睡）。不会自动重连 —— 点「打开串口并复位设备」重连。');
        setOpen(false);
      }
    };
    serial.addEventListener('disconnect', onDisconnect);
    return () => serial.removeEventListener('disconnect', onDisconnect);
  }, [supported]);

  // 卸载即恢复深睡：不走 busy 门控，直接关（busy 飞行中卸载也必须关掉端口）。
  useEffect(() => () => {
    // cleanup 不能 async：closePort 先同步抓到 reader/port 再按顺序关，
    // 尾巴上 .catch 兜底，不让同步抛出的异常逃进 React 错误边界。
    const done = closePort();
    portRef.current = null;
    readerRef.current = null;
    done.catch(() => {});
  }, []);

  const download = () => {
    // 下载头用仓库既有的本地时间格式（formatTime），不用 UTC 的 toISOString。
    const header = `# 串口日志 ${formatTime(Date.now() / 1000)} baud=${baud} lines=${lineCount}`;
    const blob = new Blob([bufferRef.current.toText({ header })], { type: 'text/plain' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `serial-${Date.now()}.log`;
    a.click();
    URL.revokeObjectURL(a.href);
  };

  if (!supported) {
    return (
      <Banner>
        这个浏览器没有 Web Serial。请用桌面版 Chrome / Edge 89+ 或 Firefox 151+
        打开本页，并且必须是 HTTPS 或 localhost（局域网 IP 的 http 页面拿不到串口）。
      </Banner>
    );
  }

  return (
    <div>
      <Banner>{err}</Banner>
      <p className="muted">
        打开串口会<strong>复位设备一次</strong>；监视期间设备不会进深睡，关闭本页即恢复。
      </p>

      <div className="row" style={{ marginTop: 12, alignItems: 'flex-end' }}>
        <label className="field" style={{ flex: '0 0 130px' }}>
          <div className="field-label">波特率</div>
          <select value={baud} disabled={open} onChange={(e) => setBaud(Number(e.target.value))}>
            {BAUD_CHOICES.map((b) => <option key={b} value={b}>{b}</option>)}
          </select>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">连接</div>
          <BusyButton busy={busy} busyText={open ? '正在断开…' : '正在打开…'} onClick={open ? disconnect : connect}>
            {open ? '断开' : '打开串口并复位设备'}
          </BusyButton>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">显示</div>
          <button type="button" className="btn secondary" onClick={() => { if (paused) scheduleRender(); setPaused((p) => !p); }}>
            {paused ? '继续显示' : '暂停显示'}
          </button>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">清屏</div>
          <button type="button" className="btn secondary" onClick={() => { bufferRef.current.clear(); scheduleRender(); }}>
            清屏
          </button>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">导出</div>
          <button type="button" className="btn secondary" onClick={download} disabled={!lineCount}>
            下载 .log
          </button>
        </label>
        <label className="field" style={{ flex: '0 0 auto' }}>
          <div className="field-label">时间戳</div>
          <span>
            <input type="checkbox" checked={stamp} onChange={(e) => setStamp(e.target.checked)} />
            每行加接收时间戳
          </span>
        </label>
        <label className="field" style={{ flex: '1 1 160px' }}>
          <div className="field-label">过滤</div>
          <input
            value={filter}
            onChange={(e) => { setFilter(e.target.value); filterRef.current = e.target.value; scheduleRender(); }}
          />
        </label>
      </div>

      <pre
        ref={preRef}
        className="mono"
        style={{ maxHeight: 420, overflow: 'auto', whiteSpace: 'pre-wrap' }}
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 8;
        }}
      >
        {text}
      </pre>
      <p className="muted">
        缓冲上限 {RING_LINE_CAP} 行；暂停只停渲染，读取仍在继续（否则设备侧写日志会被拖住）。
      </p>
    </div>
  );
}
