import i18n from "../i18n";

type Replacement = { source: string; key: string };

// 平台把画质名/提示/线路名当数据返回，值本身是中文；这里只做展示层本地化，
// 不改后端返回的原始值（线路匹配、录制参数仍然依赖它）。
const QUALITY_REPLACEMENTS: Replacement[] = [
  { source: "杜比全景声", key: "watch.qualityText.dolbyAtmos" },
  { source: "最高可用", key: "watch.qn.40000" },
  { source: "当前不可用", key: "watch.qualityUnavailable" },
  { source: "源码率", key: "watch.qualityText.sourceBitrate" },
  { source: "原画真彩", key: "watch.qualityText.originalTrueColor" },
  { source: "原画", key: "watch.qn.10000" },
  { source: "蓝光", key: "watch.qn.400" },
  { source: "超清", key: "watch.qn.250" },
  { source: "高清", key: "watch.qn.150" },
  { source: "流畅", key: "watch.qn.80" },
  { source: "标清", key: "watch.qualityText.sd" },
  { source: "杜比", key: "watch.qn.30000" },
].sort((a, b) => b.source.length - a.source.length);

const LINE_REPLACEMENTS: Replacement[] = [
  { source: "默认线路", key: "watch.lineDefault" },
  { source: "阿里云", key: "watch.lineVendor.aliyun" },
  { source: "腾讯云", key: "watch.lineVendor.tencent" },
  { source: "华为云", key: "watch.lineVendor.huawei" },
  { source: "网宿", key: "watch.lineVendor.wangsu" },
  { source: "线路", key: "watch.line" },
].sort((a, b) => b.source.length - a.source.length);

function isEnglish() {
  return (i18n.resolvedLanguage ?? i18n.language ?? "").toLowerCase().startsWith("en");
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function replaceTerms(value: string | null | undefined, terms: Replacement[]) {
  const text = value ?? "";
  if (!text || !isEnglish()) return text;
  const pattern = new RegExp(terms.map((term) => escapeRegExp(term.source)).join("|"), "g");
  return text.replace(pattern, (match: string, offset: number, whole: string) => {
    const term = terms.find((item) => item.source === match);
    if (!term) return match;
    const translated = String(i18n.t(term.key));
    // “蓝光60帧” -> “Blu-ray 60帧”：译文后紧跟数字/字母时补一个空格。
    const next = whole.charAt(offset + match.length);
    return /[0-9A-Za-z]/.test(next) ? `${translated} ` : translated;
  });
}

/** 展示用画质名/提示：英文界面把平台中文名映射成 i18n 文案，中文界面原样返回。 */
export function qualityText(value: string | null | undefined) {
  return replaceTerms(value, QUALITY_REPLACEMENTS);
}

/** 展示用线路名：保留原始 value 用于匹配，只在 label/hint 上做本地化。 */
export function lineText(value: string | null | undefined) {
  return replaceTerms(value, LINE_REPLACEMENTS);
}

/** 给不可用画质的 hint 补“当前不可用”，并去掉与画质名重复的前缀。 */
export function unavailableQualityHint(
  label: string | null | undefined,
  hint: string | null | undefined,
  unavailableLabel: string,
) {
  const labelText = qualityText(label);
  let hintText = qualityText(hint);
  if (hintText.includes(unavailableLabel)) return hintText;
  if (hintText === labelText) hintText = "";
  else if (labelText && hintText.startsWith(`${labelText} · `)) {
    hintText = hintText.slice(labelText.length + 3);
  }
  return hintText ? `${hintText} · ${unavailableLabel}` : unavailableLabel;
}
