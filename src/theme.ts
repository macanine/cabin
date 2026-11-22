// Mantine shares the app's type scale, colour palette and short transitions.
import {
  Alert, Badge, Combobox, createTheme, Drawer, Menu, Modal, Popover, SegmentedControl, Switch, Tooltip,
  type MantineColorsTuple,
} from "@mantine/core";

/** The accent (#c2622d, styles/base.css --accent) as Mantine's primary. */
const brand: MantineColorsTuple = [
  "#fdf3ed", "#f9e2d4", "#f2c2a6", "#eaa175", "#e2844d", "#d97036", "#c2622d", "#a65225", "#87421d", "#693314",
];

const floating = { transition: "fade", duration: 120, exitDuration: 90, timingFunction: "ease-out" } as const;
const dialog = { transition: "fade", duration: 160, exitDuration: 110, timingFunction: "ease-out" } as const;
const overlay = { backgroundOpacity: 0.26 };

export const theme = createTheme({
  primaryColor: "brand",
  primaryShade: { light: 6, dark: 4 },
  colors: { brand },
  fontFamily: "var(--font)",
  fontFamilyMonospace: "var(--mono)",
  headings: { fontFamily: "var(--font)", fontWeight: "600" },
  defaultRadius: "md",
  cursorType: "pointer",
  focusRing: "auto",
  respectReducedMotion: true,
  // Controls stay in place when pressed.
  activeClassName: "",
  fontSizes: { xs: "11.5px", sm: "12.5px", md: "13.5px", lg: "16px", xl: "20px" },
  radius: { xs: "4px", sm: "6px", md: "8px", lg: "14px", xl: "18px" },
  shadows: {
    xs: "0 1px 2px rgb(0 0 0 / 0.08)",
    sm: "0 2px 6px rgb(0 0 0 / 0.10)",
    md: "0 4px 12px rgb(0 0 0 / 0.12)",
    lg: "0 6px 20px rgb(0 0 0 / 0.14)",
    xl: "0 10px 28px rgb(0 0 0 / 0.16)",
  },
  components: {
    Badge: Badge.extend({ defaultProps: { radius: "sm" } }),
    Alert: Alert.extend({ defaultProps: { variant: "light" } }),
    SegmentedControl: SegmentedControl.extend({ defaultProps: { transitionDuration: 140 } }),
    Switch: Switch.extend({ defaultProps: { size: "sm" } }),
    Modal: Modal.extend({
      defaultProps: { centered: true, radius: "lg", shadow: "lg", overlayProps: overlay, transitionProps: dialog },
    }),
    Drawer: Drawer.extend({ defaultProps: { shadow: "lg", overlayProps: overlay, transitionProps: dialog } }),
    Menu: Menu.extend({ defaultProps: { transitionProps: floating, shadow: "md", offset: 6 } }),
    Popover: Popover.extend({ defaultProps: { transitionProps: floating, shadow: "md" } }),
    Combobox: Combobox.extend({ defaultProps: { transitionProps: floating, shadow: "md", offset: 6 } }),
    Tooltip: Tooltip.extend({
      defaultProps: { openDelay: 350, radius: "sm", transitionProps: { transition: "fade", duration: 100 } },
    }),
  },
});
