// IAI Bridge — popup: paste/save the iai token, show connection state, reconnect.

const tokenEl = document.getElementById("token");
const statusEl = document.getElementById("status");
const connEl = document.getElementById("conn");

function refreshConn() {
  chrome.runtime.sendMessage({ type: "getStatus" }, (resp) => {
    const ok = resp && resp.connected;
    connEl.textContent = ok ? "Da ket noi IAI" : "Chua ket noi IAI";
    connEl.style.color = ok ? "#0a6" : "#888";
    if (resp && resp.status) statusEl.textContent = resp.status;
  });
}

chrome.storage.local.get("token").then(({ token }) => {
  if (token) tokenEl.value = token;
});
refreshConn();
setInterval(refreshConn, 1000);

document.getElementById("save").addEventListener("click", async () => {
  const token = tokenEl.value.trim();
  await chrome.storage.local.set({ token });
  chrome.runtime.sendMessage({ type: "reconnect" });
  statusEl.textContent = token ? "Đã lưu token, đang kết nối…" : "Token trống.";
  setTimeout(refreshConn, 400);
});

document.getElementById("reconnect").addEventListener("click", () => {
  chrome.runtime.sendMessage({ type: "reconnect" });
  statusEl.textContent = "Đang kết nối lại…";
  setTimeout(refreshConn, 400);
});

