import { ChevronLeft, ChevronRight } from "lucide-react";
import { useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { Checkbox } from "../../components/arc/checkbox/checkbox";
import { Button, Segmented, cx } from "../../components/ui/Controls";
import { SelectMenu } from "../../components/ui/SelectMenu";
import {
  useDanmakuPrefs,
  type DanmakuFontFamily,
  type DanmakuOutline,
} from "../../lib/danmakuPrefs";

type RangeProps = {
  value: number;
  min: number;
  max: number;
  step: number;
  label: string;
  onChange: (value: number) => void;
};

function PanelRange({ value, min, max, step, label, onChange }: RangeProps) {
  const progress = max > min ? ((value - min) / (max - min)) * 100 : 0;
  return (
    <div className="hv-range dm-settings__range">
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        aria-label={label}
        onChange={(event) => onChange(Number(event.target.value))}
        className="hv-range__input"
        style={{ "--progress": `${progress}%` } as CSSProperties}
      />
    </div>
  );
}

type SliderRowProps = Omit<RangeProps, "label"> & {
  label: string;
  valueLabel: string;
};

function SliderRow({ label, valueLabel, ...range }: SliderRowProps) {
  return (
    <div className="dm-settings__row">
      <span className="dm-settings__label">{label}</span>
      <PanelRange {...range} label={label} />
      <span className="dm-settings__value">{valueLabel}</span>
    </div>
  );
}

const FONT_OPTIONS: { value: DanmakuFontFamily; labelKey: string }[] = [
  { value: "sans", labelKey: "watch.danmakuFontSans" },
  { value: "serif", labelKey: "watch.danmakuFontSerif" },
  { value: "kai", labelKey: "watch.danmakuFontKai" },
];

const OUTLINE_OPTIONS: { value: DanmakuOutline; labelKey: string }[] = [
  { value: "heavy", labelKey: "watch.danmakuOutlineHeavy" },
  { value: "outline", labelKey: "watch.danmakuOutlineOutline" },
  { value: "shadow45", labelKey: "watch.danmakuOutlineShadow" },
];

export default function DanmakuSettingsPanel() {
  const { t } = useTranslation();
  const prefs = useDanmakuPrefs();
  const [advanced, setAdvanced] = useState(false);

  if (advanced) {
    return (
      <section className="dm-settings" role="dialog" aria-label={t("watch.danmakuAdvancedTitle")}>
        <button
          type="button"
          className="dm-settings__header"
          onClick={() => setAdvanced(false)}
        >
          <ChevronLeft size={16} strokeWidth={1.8} aria-hidden />
          <span>{t("watch.danmakuAdvancedTitle")}</span>
        </button>

        <div className="dm-settings__advanced">
          <div className="dm-settings__block">
            <div className="dm-settings__field-label">{t("watch.danmakuFontFamily")}</div>
            <div className="dm-settings__font-row">
              <SelectMenu
                value={prefs.fontFamily}
                ariaLabel={t("watch.danmakuFontFamily")}
                options={FONT_OPTIONS.map((option) => ({
                  value: option.value,
                  label: t(option.labelKey),
                }))}
                onChange={(value) =>
                  prefs.set({ fontFamily: value as DanmakuFontFamily })
                }
                className="w-[154px]"
              />
              <div className={cx("dm-settings__checkbox-row dm-settings__bold", prefs.bold && "is-checked")}>
                <Checkbox
                  checked={prefs.bold}
                  onCheckedChange={(next) => prefs.set({ bold: next === true })}
                  label={t("watch.danmakuBold")}
                />
              </div>
            </div>
          </div>

          <div className="dm-settings__block">
            <div className="dm-settings__field-label">{t("watch.danmakuOutline")}</div>
            <Segmented
              fullWidth
              value={prefs.outline}
              onChange={(next) => prefs.set({ outline: next })}
              items={OUTLINE_OPTIONS.map((option) => ({
                key: option.value,
                label: t(option.labelKey),
              }))}
            />
          </div>
        </div>

        <div className="dm-settings__advanced-footer">
          <Button size="sm" variant="ghost" onClick={() => prefs.reset()}>
            {t("watch.danmakuResetDefaults")}
          </Button>
        </div>
      </section>
    );
  }

  return (
    <section className="dm-settings" role="dialog" aria-label={t("watch.danmakuSettings")}>
      <div className="dm-settings__body">
        <SliderRow
          label={t("watch.danmakuArea")}
          value={prefs.area}
          min={0.3}
          max={1}
          step={0.05}
          valueLabel={`${Math.round(prefs.area * 100)}%`}
          onChange={(next) => prefs.set({ area: next })}
        />

        <div className="dm-settings__block">
          <div className="dm-settings__field-label">{t("watch.danmakuDensity")}</div>
          <Segmented
            fullWidth
            value={prefs.density < 1.15 ? "normal" : prefs.density < 1.6 ? "more" : "overlap"}
            onChange={(next) =>
              prefs.set({
                density: next === "normal" ? 1 : next === "more" ? 1.35 : 1.8,
              })
            }
            items={[
              { key: "normal", label: t("watch.danmakuDensityNormal") },
              { key: "more", label: t("watch.danmakuDensityMore") },
              { key: "overlap", label: t("watch.danmakuDensityOverlap") },
            ]}
          />
        </div>

        <SliderRow
          label={t("watch.danmakuOpacity")}
          value={prefs.opacity}
          min={0.3}
          max={1}
          step={0.05}
          valueLabel={`${Math.round(prefs.opacity * 100)}%`}
          onChange={(next) => prefs.set({ opacity: next })}
        />

        <SliderRow
          label={t("watch.danmakuFont")}
          value={prefs.fontScale}
          min={0.75}
          max={1.6}
          step={0.05}
          valueLabel={`${Math.round(prefs.fontScale * 100)}%`}
          onChange={(next) => prefs.set({ fontScale: next })}
        />

        <SliderRow
          label={t("watch.danmakuSpeed")}
          value={prefs.speed}
          min={0.5}
          max={2}
          step={0.1}
          valueLabel={
            prefs.speed < 0.85
              ? t("watch.danmakuSpeedSlow")
              : prefs.speed > 1.15
                ? t("watch.danmakuSpeedFast")
                : t("watch.danmakuSpeedMedium")
          }
          onChange={(next) => prefs.set({ speed: next })}
        />

        <div className="dm-settings__quick">
          <div className={cx("dm-settings__checkbox-row", prefs.scaleWithScreen && "is-checked")}>
            <Checkbox
              checked={prefs.scaleWithScreen}
              onCheckedChange={(next) => prefs.set({ scaleWithScreen: next === true })}
              label={t("watch.danmakuScaleWithScreen")}
            />
          </div>
        </div>
      </div>

      <button type="button" className="dm-settings__footer" onClick={() => setAdvanced(true)}>
        <span>{t("watch.danmakuAdvanced")}</span>
        <ChevronRight size={16} strokeWidth={1.7} aria-hidden />
      </button>
    </section>
  );
}
