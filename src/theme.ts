// Mantine shares the app's type scale, colour palette and short transitions.
import {
  Alert, Badge, Combobox, createTheme, Drawer, Menu, Modal, Popover, SegmentedControl, Switch, Tooltip,
  type MantineColorsTuple,
} from "@mantine/core";

/** The accent (styles/base.css --accent-fill) as Mantine's primary. */
const brand: MantineColorsTuple = [
  "#eef7f4", "#d8ece6", "#b1d9ce", "#86c5b4", "#6cc3ac", "#3b917f", "#1c6a5a", "#17594c", "#12473d", "#0d352d",
];

const floating = {
  transition: {
    transitionProperty: "opacity, transform",
    in: { opacity: 1, transform: "none" },
    out: { opacity: 0, transform: "translate(var(--floating-x, 0px), var(--floating-y, 0px)) scale(0.98)" },
  },
  duration: 170,
  exitDuration: 95,
  timingFunction: "var(--ease-out)",
} as const;
const dialog = {
  transition: {
    transitionProperty: "opacity, transform",
    in: { opacity: 1, transform: "none" },
    out: { opacity: 0, transform: "translateY(6px) scale(0.985)" },
  },
  duration: 220,
  exitDuration: 120,
  timingFunction: "var(--ease-spring)",
} as const;
const drawer = {
  transition: {
    transitionProperty: "opacity, transform",
    in: { opacity: 1, transform: "none" },
    out: { opacity: 0, transform: "translate(var(--drawer-x, 0px), var(--drawer-y, 0px))" },
  },
  duration: 240,
  exitDuration: 130,
  timingFunction: "var(--ease-out)",
} as const;
const overlay = { backgroundOpacity: 0.26, transitionProps: { duration: 160, exitDuration: 110 } };

export const theme = createTheme({
  primaryColor: "brand",
  primaryShade: 6,
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
    SegmentedControl: SegmentedControl.extend({
      defaultProps: { transitionDuration: 260, transitionTimingFunction: "var(--ease-spring)" },
    }),
    Switch: Switch.extend({ defaultProps: { size: "sm" } }),
    Modal: Modal.extend({
      defaultProps: { centered: true, radius: "lg", shadow: "lg", overlayProps: overlay, transitionProps: dialog },
    }),
    Drawer: Drawer.extend({
      defaultProps: { shadow: "lg", overlayProps: overlay, transitionProps: drawer },
      classNames: (_, { position }) => ({ content: `drawer-${position ?? "left"}` }),
    }),
    Menu: Menu.extend({ defaultProps: { transitionProps: floating, shadow: "md", offset: 6, radius: "lg" } }),
    Popover: Popover.extend({ defaultProps: { transitionProps: floating, shadow: "md", radius: "lg" } }),
    Combobox: Combobox.extend({ defaultProps: { transitionProps: floating, shadow: "md", offset: 6 } }),
    Tooltip: Tooltip.extend({
      defaultProps: { openDelay: 350, radius: "sm", transitionProps: { transition: "fade", duration: 100 } },
    }),
  },
});
