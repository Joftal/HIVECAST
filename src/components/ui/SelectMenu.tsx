import { Check } from "lucide-react";
import { DropdownMenu, type DropdownItem, type DropdownMenuProps } from "../arc/dropdown-menu/dropdown-menu";

export type SelectOption = {
  value: string;
  label: string;
  hint?: string;
  disabled?: boolean;
};

type Props = Omit<DropdownMenuProps, "label" | "items" | "icon"> & {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** 当前值不在 options 里时的兜底文案。 */
  placeholder?: string;
  icon?: DropdownMenuProps["icon"];
};

/** 把「当前值 + 选项」的选择菜单收口到 Arc DropdownMenu：选中的项带勾，其余留出同宽空位。 */
export function SelectMenu({ value, options, onChange, placeholder, icon, ariaLabel, ...rest }: Props) {
  const current = options.find((option) => option.value === value);
  const items: DropdownItem[] = options.map((option) => ({
    key: option.value,
    label: option.label,
    hint: option.hint,
    disabled: option.disabled,
    icon: option.value === value ? <Check size={15} strokeWidth={2} /> : <span />,
    onSelect: () => onChange(option.value),
  }));

  return (
    <DropdownMenu
      label={current?.label ?? placeholder ?? ""}
      items={items}
      icon={icon}
      morphLabel={false}
      ariaLabel={ariaLabel ? (current ? `${ariaLabel}: ${current.label}` : ariaLabel) : undefined}
      {...rest}
    />
  );
}

export default SelectMenu;
