import React, { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { setToken } from '../auth.js';
import { api } from '../api.js';

export default function Login() {
  const [token, setTok] = useState('');
  const [err, setErr] = useState('');
  const nav = useNavigate();

  const submit = async (e) => {
    e.preventDefault();
    setErr('');
    if (!token.trim()) { setErr('请输入 OPERATOR_TOKEN'); return; }
    setToken(token.trim());
    try {
      await api.devices();
      nav('/devices');
    } catch (err) {
      clearInvalidToken();
      setErr('token 无效: ' + err.message);
    }
  };
  return (
    <div className="login-wrap">
      <form className="card login-card" onSubmit={submit}>
        <h1>Youn Ink Admin</h1>
        <p className="muted">输入服务端 OPERATOR_TOKEN 登录</p>
        <input
          type="password"
          value={token}
          onChange={(e) => setTok(e.target.value)}
          placeholder="OPERATOR_TOKEN"
          style={{ width: '100%', margin: '12px 0' }}
        />
        {err && <div className="err">{err}</div>}
        <button type="submit" className="btn" style={{ width: '100%' }}>登录</button>
      </form>
    </div>
  );
}
function clearInvalidToken() { localStorage.removeItem('youn_operator_token'); }
