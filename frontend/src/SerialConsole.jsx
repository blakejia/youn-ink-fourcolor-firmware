import React, { useEffect, useRef, useState } from 'react';
import { Banner, BusyButton } from './ui.jsx';
import { LineDecoder, RingBuffer, RING_LINE_CAP } from './serialLog.js';

const BAUD_CHOICES = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];

export default function SerialConsole() {
  const [supported] = useState(() => typeof navigator !== 'undefined' && !!navigator.serial);
  const [baud, setBaud] = useState(115200);
  const [open, setOpen] = useState(false);
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
      const el = preRef.current;
      if (el && stickRef.current) el.scrollTop = el.scrollHeight;
    });
  };

  // 单读者：同一时刻只有一个 reader。简报原稿的 while(port.readable && readerRef.current)
  // 入口条件在 connect 刚调用时恒为假（readerRef 还没赋值），这里改成直进式。
  const readLoop = async (port) => {
    const reader = port.readable.getReader();
    readerRef.current = reader;
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        const lines = decoderRef.current.push(value);
        if (lines.length) {
          if (stampRef.current) {
            const t = new Date().toISOString().slice(11, 23);
            bufferRef.current.push(lines.map((l) => `[${t}] ${l}`));
          } else {
            bufferRef.current.push(lines);
          }
          if (!pausedRef.current) scheduleRender();
        }
      }
    } catch (e) {
      // 手动断开 / 设备掉线已经清过 portRef 并写过更准的提示，不覆盖
      if (portRef.current === port) setErr(`读取中断：${e.message}`);
    } finally {
      reader.releaseLock();
      if (readerRef.current === reader) readerRef.current = null;
    }
  };

  const connect = async () => {
    setErr('');
    try {
      const port = await navigator.serial.requestPort();
      await port.open({ baudRate: baud });
      portRef.current = port;
      setOpen(true);
      // 打开会复位设备：清空旧内容，从头开始看
      bufferRef.current.clear();
      decoderRef.current = new LineDecoder();
      scheduleRender();
      readLoop(port);
    } catch (e) {
      setErr(`打开失败：${e.message}（本机另一个程序可能占着这个端口）`);
    }
  };

  const disconnect = async () => {
    try {
      await readerRef.current?.cancel();
    } catch (e) { /* 已经断了 */ }
    try {
      await portRef.current?.close();
    } catch (e) { /* 已经断了 */ }
    // 解码器会把以孤立 \r 结尾的半行挂起：停止时必须 flush 出来，否则丢一行。
    // flush 之后此实例不再复用 —— 下次 connect 会新建 LineDecoder
    //（旧实例的 TextDecoder 内部可能残留多字节序列的暂存字节）。
    try {
      const tail = decoderRef.current.flush();
      if (tail.length) {
        bufferRef.current.push(tail);
        if (!pausedRef.current) scheduleRender();
      }
    } catch (e) { /* 无内容可刷 */ }
    portRef.current = null;
    readerRef.current = null;
    setOpen(false);
  };

  useEffect(() => {
    // serial 先抓到局部变量：挂载后若有人把 navigator.serial 删掉
    //（降级测试即如此），cleanup 里再读 navigator.serial 会崩。
    const serial = typeof navigator !== 'undefined' ? navigator.serial : undefined;
    if (!supported || !serial) return undefined;
    const onDisconnect = (e) => {
      if (portRef.current && e.target === portRef.current) {
        // 设备掉线同样先把挂起的半行刷出来（此实例不再复用，重连时新建）。
        try {
          const tail = decoderRef.current.flush();
          if (tail.length) {
            bufferRef.current.push(tail);
            scheduleRender();
          }
        } catch (err2) { /* 无内容可刷 */ }
        setErr('设备已断开（USB 掉线或设备进入深睡）。不会自动重连 —— 点「打开串口并复位设备」重连。');
        setOpen(false);
        portRef.current = null;
      }
    };
    serial.addEventListener('disconnect', onDisconnect);
    return () => serial.removeEventListener('disconnect', onDisconnect);
  }, [supported]);

  useEffect(() => () => { disconnect(); }, []);

  const download = () => {
    const header = `# 串口日志 ${new Date().toISOString()} baud=${baud} lines=${lineCount}`;
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

      <div className="field">
        <label>
          波特率
          <select value={baud} disabled={open} onChange={(e) => setBaud(Number(e.target.value))}>
            {BAUD_CHOICES.map((b) => <option key={b} value={b}>{b}</option>)}
          </select>
        </label>
        <BusyButton busy={false} onClick={open ? disconnect : connect}>
          {open ? '断开' : '打开串口并复位设备'}
        </BusyButton>
        <button type="button" className="btn secondary" onClick={() => setPaused((p) => !p)}>
          {paused ? '继续显示' : '暂停显示'}
        </button>
        <button type="button" className="btn secondary" onClick={() => { bufferRef.current.clear(); scheduleRender(); }}>
          清屏
        </button>
        <button type="button" className="btn secondary" onClick={download} disabled={!lineCount}>
          下载 .log
        </button>
        <label>
          <input type="checkbox" checked={stamp} onChange={(e) => setStamp(e.target.checked)} />
          每行加接收时间戳
        </label>
        <label>
          过滤
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
