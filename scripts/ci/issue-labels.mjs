export function formLabels(body) {
  const sections = new Map();
  let currentHeading = null;
  for (const rawLine of body.split(/\r?\n/)) {
    const m = /^###\s+(.+?)\s*$/.exec(rawLine);
    if (m) {
      currentHeading = m[1];
      sections.set(currentHeading, []);
    } else if (currentHeading) {
      sections.get(currentHeading).push(rawLine.trim());
    }
  }
  const getVal = (...headings) => {
    for (const h of headings) {
      const lines = sections.get(h);
      if (lines) {
        const joined = lines.filter(Boolean).join(" ");
        if (joined && joined !== "_No response_") return joined;
      }
    }
    return "";
  };

  const toAdd = new Set();

  const platform = getVal("涉及直播平台", "异常平台");
  if (platform.includes("Bilibili") || platform.includes("哔哩哔哩")) toAdd.add("platform: bilibili");
  if (platform.includes("Douyu") || platform.includes("斗鱼")) toAdd.add("platform: douyu");
  if (platform.includes("Huya") || platform.includes("虎牙")) toAdd.add("platform: huya");
  if (platform.includes("Douyin") || platform.includes("抖音")) toAdd.add("platform: douyin");

  const os = getVal("操作系统");
  if (os.includes("macOS")) toAdd.add("os: macos");
  if (os.includes("Windows")) toAdd.add("os: windows");

  const area = getVal("涉及功能模块", "异常环节", "建议所属模块");
  if (area.includes("监控") || area.includes("录制")) toAdd.add("area: recorder");
  if (area.includes("弹幕")) toAdd.add("area: danmaku");
  if (area.includes("合并") || area.includes("后处理") || area.includes("转封装")) toAdd.add("area: postprocess");
  if (area.includes("回放") || area.includes("画廊")) toAdd.add("area: playback");
  if (area.includes("账号") || area.includes("凭据")) toAdd.add("area: auth");
  if (area.includes("安装器") || area.includes("更新") || area.includes("启动")) toAdd.add("area: installer");
  if (area.includes("界面") || area.includes("国际化")) toAdd.add("area: ui");

  return toAdd;
}

export async function reconcileLabels(issueUrl, previousBody, headers, request = fetch) {
  const derived = formLabels(previousBody || "");
  const url = issueUrl + "/labels";
  // A later edit may land while the request is in flight. Check the live form after writes.
  for (let attempt = 0; attempt < 5; attempt++) {
    const response = await request(issueUrl, { headers });
    if (!response.ok) throw new Error("Read issue: HTTP " + response.status);
    const { body, labels } = await response.json();
    const wanted = formLabels(body || "");
    const existing = new Set(labels.map(label => label.name));
    const removed = [...derived].filter(label => existing.has(label) && !wanted.has(label));
    const added = [...wanted].filter(label => !existing.has(label));
    if (!removed.length && !added.length) return;

    for (const label of wanted) derived.add(label);
    for (const label of removed) {
      const result = await request(url + "/" + encodeURIComponent(label), { method: "DELETE", headers });
      if (!result.ok && result.status !== 404) throw new Error("Remove label: HTTP " + result.status);
    }
    if (added.length) {
      const result = await request(url, { method: "POST", headers, body: JSON.stringify({ labels: added }) });
      if (!result.ok) throw new Error("Add labels: HTTP " + result.status);
    }
  }
  throw new Error("Issue labels are still changing after five reconciliation attempts");
}
