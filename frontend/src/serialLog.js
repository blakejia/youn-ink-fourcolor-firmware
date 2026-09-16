// 串口字节流 → 行。三条规则都来自真机教训：
//  1) 一个多字节字符可能被拆进两个 USB 包 ⇒ TextDecoder 必须 stream:true
//  2) 进度行用 \r 刷新 ⇒ \r 也结束一行，否则永远不刷新
//  3) ESP-IDF 开 CONFIG_LOG_COLORS 时带 ANSI 色码 ⇒ 渲染前剥掉

export const RING_LINE_CAP = 5000;
export const RING_BYTE_CAP = 2 * 1024 * 1024;

const ANSI = /\x1b\[[0-9;]*m/g;
// 保留 \t，丢掉其它 C0 控制字符（\n \r 在分行阶段已消费）
const CTRL = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g;

// 复用同一个 encoder：环形缓冲按真实 UTF-8 字节计账
const utf8 = new TextEncoder();
const sizeOf = (line) => utf8.encode(line).length + 1;

export class LineDecoder {
  constructor() {
    this.decoder = new TextDecoder('utf-8', { fatal: false });
    this.pending = '';
    // 上一包是否以**孤立** \r 结尾（即 \r 正好是当时缓冲区的最后一个字符）
    this.sawCR = false;
  }

  /** @param {Uint8Array} bytes @returns {string[]} 完整行 */
  push(bytes) {
    this.pending += this.decoder.decode(bytes, { stream: true });
    // 上一包以孤立 \r 结尾、这一包以 \n 开头 ⇒ 它们是同一个 CRLF，吞掉这个 \n，
    // 否则 USB 分块边界随机时每拆一次就多一个空行（ESP-IDF 日志行以 CRLF 结尾）。
    if (this.sawCR && this.pending[0] === '\n') this.pending = this.pending.slice(1);
    this.sawCR = false;
    const out = [];
    let idx;
    while ((idx = this.pending.search(/[\r\n]/)) !== -1) {
      const line = this.pending.slice(0, idx);
      const isCR = this.pending[idx] === '\r';
      // CRLF 是一个断行，不是两个：否则每行后面都会多一个空行
      let next = idx + 1;
      if (isCR && this.pending[next] === '\n') next += 1;
      // 孤立 \r 正好落在缓冲区末尾 ⇒ 它可能与下一包的 \n 组成 CRLF，留给下一包裁决
      this.sawCR = isCR && next === this.pending.length;
      this.pending = this.pending.slice(next);
      out.push(clean(line));
    }
    return out;
  }

  flush() {
    this.sawCR = false;
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
      this.bytes += sizeOf(l);
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
      this.bytes -= sizeOf(dropped);
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
