import React, { useEffect, useState } from 'react';
import { useBlocker } from 'react-router-dom';
import SerialConsole from '../SerialConsole.jsx';
import FirmwareFlash from '../FirmwareFlash.jsx';
import { ConfirmDialog } from '../ui.jsx';
import { registerUnsavedCheck } from '../unsaved.js';

const TABS = [
  { id: 'console', label: '串口监视' },
  { id: 'flash', label: '固件刷写' },
];

export default function Serial() {
  const [tab, setTab] = useState('console');
  // 刷写中切页签 = 把 FirmwareFlash 卸载 = 掐断刷写留下半写分区：busy 时禁用另一个页签。
  const [flashBusy, setFlashBusy] = useState(false);

  // SPA 内路由跳转用与 Pages.jsx 同一套守卫：useBlocker 拦路由，
  // registerUnsavedCheck 拦顶栏设备切换（App.jsx DeviceSelector 经 hasUnsaved 询问）。
  const blocker = useBlocker(flashBusy);
  useEffect(() => {
    registerUnsavedCheck(() => flashBusy);
    return () => registerUnsavedCheck(null);
  }, [flashBusy]);

  const tryTab = (id) => {
    if (flashBusy && id !== 'flash') return;
    setTab(id);
  };

  return (
    <div>
      <h1>串口 / 固件</h1>
      <p className="muted">
        串口由<strong>你的浏览器</strong>直接打开（WebSerial），不经服务端。
        所以本页必须运行在<strong>设备所插的那台机器</strong>上，且使用桌面
        Chrome / Edge 89+ 或 Firefox 151+。
      </p>
      {flashBusy && (
        <p className="muted">
          正在刷写固件：已锁定页签切换与离开本页，切勿拔线或关闭页面。
        </p>
      )}

      <div className="row" role="group" aria-label="串口工具">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            aria-pressed={tab === t.id}
            className={tab === t.id ? 'btn' : 'btn secondary'}
            onClick={() => tryTab(t.id)}
            disabled={flashBusy && t.id !== 'flash'}
            title={flashBusy && t.id !== 'flash' ? '刷写进行中，完成后才能切换' : undefined}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="card">
        {tab === 'console' ? <SerialConsole /> : <FirmwareFlash onBusyChange={setFlashBusy} />}
      </div>

      {blocker.state === 'blocked' && (
        <ConfirmDialog
          title="正在刷写固件"
          body="现在离开会掐断刷写，设备可能处于半写状态（变砖）。请等待刷写完成。"
          confirmLabel="仍要离开（可能变砖）"
          cancelLabel="留在本页"
          danger
          onConfirm={() => blocker.proceed()}
          onCancel={() => blocker.reset()}
        />
      )}
    </div>
  );
}
