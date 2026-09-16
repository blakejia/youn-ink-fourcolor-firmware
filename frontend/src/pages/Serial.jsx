import React, { useState } from 'react';
import SerialConsole from '../SerialConsole.jsx';
import FirmwareFlash from '../FirmwareFlash.jsx';

const TABS = [
  { id: 'console', label: '串口监视' },
  { id: 'flash', label: '固件刷写' },
];

export default function Serial() {
  const [tab, setTab] = useState('console');

  return (
    <div>
      <h1>串口 / 固件</h1>
      <p className="muted">
        串口由<strong>你的浏览器</strong>直接打开（WebSerial），不经服务端。
        所以本页必须运行在<strong>设备所插的那台机器</strong>上，且使用桌面
        Chrome / Edge 89+ 或 Firefox 151+。
      </p>

      <div className="row" role="group" aria-label="串口工具">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            aria-pressed={tab === t.id}
            className={tab === t.id ? 'btn' : 'btn secondary'}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="card">
        {tab === 'console' ? <SerialConsole /> : <FirmwareFlash />}
      </div>
    </div>
  );
}
