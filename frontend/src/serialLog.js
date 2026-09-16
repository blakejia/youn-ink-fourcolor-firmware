// 串口字节流 → 行。三条规则都来自真机教训：
//  1) 一个多字节字符可能被拆进两个 USB 包 ⇒ TextDecoder 必须 stream:true
//  2) 进度行用 \r 刷新 ⇒ \r 也结束一行，否则永远不刷新
//  3) ESP-IDF 开 CONFIG_LOG_COLORS 时带 ANSI 色码 ⇒ 渲染前剥掉

export const RING_LINE_CAP = 5000;
export const RING_BYTE_CAP = 2 * 1024 * 1024;

const ANSI = /\x1b\[[0-9;]*m/g;
// 保留 \t，丢掉其它 C0 控制字符（\n \r 在分行阶段已消费）
const CTRL = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g;

export class LineDecoder {
  constructor() {
    this.decoder = new TextDecoder('utf-8', { fatal: false });
    this.pending = '';
  }

  /** @param {Uint8Array} bytes @returns {string[]} 完整行 */
  push(bytes) {
    this.pending += this.decoder.decode(bytes, { stream: true });
    const out = [];
    let idx;
    while ((idx = this.pending.search(/[\r\n]/)) !== -1) {
      const line = this.pending.slice(0, idx);
      // CRLF 是一个断行，不是两个：否则每行后面都会多一个空行
      let next = idx + 1;
      if (this.pending[idx] === '\r' && this.pending[next] === '\n') next += 1;
      this.pending = this.pending.slice(next);
      out.push(clean(line));
    }
    return out;
  }

  flush() {
    if (!this.pending) return [];
    const line = clean(this.pending);
    this.pending = '';
    return [line];
  }
}

function clean(line) {
  return line.replace(ANSI, '').replace(CTRL, '');
}

export class RingBuffer {
  constructor() {
    this.lines = [];
    this.bytes = 0;
    this.truncated = false;
  }

  push(lines) {
    for (const l of lines) {
      this.lines.push(l);
      this.bytes += l.length + 1;
    }
    this.enforce();
  }

  pushRaw(text) {
    for (const l of text.split('\n')) this.push([l]);
  }

  enforce() {
    while (
      this.lines.length > RING_LINE_CAP ||
      (this.bytes > RING_BYTE_CAP && this.lines.length > 1)
    ) {
      const dropped = this.lines.shift();
      this.bytes -= dropped.length + 1;
      this.truncated = true;
    }
  }

  clear() {
    this.lines = [];
    this.bytes = 0;
    this.truncated = false;
  }

  /** @param {{cap?: number, header?: string}} opts @returns {string} */
  toText({ cap = Infinity, header = '' } = {}) {
    const tail = cap === Infinity ? this.lines : this.lines.slice(-cap);
    const parts = [];
    if (header) {
      parts.push(header);
      if (this.truncated) parts.push('（更早的行已被丢弃）');
    }
    return parts.concat(tail).join('\n');
  }
}
