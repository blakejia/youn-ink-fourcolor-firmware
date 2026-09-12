import React, { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { setToken, clearToken } from '../auth.js';
import { api } from '../api.js';
import { Banner, BusyButton } from '../ui.jsx';

export default function Login() {
  const [token, setTok] = useState('');
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);
  const nav = useNavigate();

  const submit = async (e) => {
    e.preventDefault();
    setErr('');
    if (!token.trim()) {
      setErr('请输入 OPERATOR_TOKEN');
      return;
    }
    setBusy(true);
    setToken(token.trim());
    try {
      await api.devices();
      nav('/devices');
    } catch (e) {
      clearToken();
      setErr('token 无效：' + e.message);
      setBusy(false);
    }
  };

  return (
    <div className="login-wrap">
      <form className="card login-card" onSubmit={submit} noValidate>
        <h1>Youn Ink Admin</h1>
        <p className="muted" style={{ marginBottom: 12 }}>输入服务端 OPERATOR_TOKEN 登录</p>
        <label className="field">
          <div className="field-label">OPERATOR_TOKEN</div>
          <input
            type="password"
            name="operator_token"
            value={token}
            onChange={(e) => setTok(e.target.value)}
            placeholder="32 位十六进制，如 8632ea0d…"
            autoComplete="off"
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            autoFocus
            disabled={busy}
            aria-invalid={err ? true : undefined}
          />
        </label>
        <Banner>{err}</Banner>
        <BusyButton busy={busy} busyText="验证中…" type="submit" style={{ width: '100%', marginTop: 12, justifyContent: 'center' }}>
          登录
        </BusyButton>
      </form>
    </div>
  );
}
