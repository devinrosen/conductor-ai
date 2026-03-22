/**
 * Conductor heritage railway themes — seven differentiation layers.
 *
 * Each theme defines not just colors, but typography, surface textures,
 * spacing rhythm, motion character, border treatment, and ornamental details.
 */

export interface ThemePalette {
  gray50: string; gray100: string; gray200: string; gray300: string;
  gray400: string; gray500: string; gray600: string; gray700: string;
  gray800: string; gray900: string; gray950: string; white: string;
  accent: string; accentGlow: string; accentBg: string;
  statusGo: string; statusCaution: string; statusStop: string;
}

export interface ThemeTypography {
  headingFamily: string;
  headingWeight: string;
  headingLetterSpacing: string;
  bodyFamily: string;
  bodySize: string;
  bodyLineHeight: string;
  labelCase: "uppercase" | "title-case" | "normal";
  labelLetterSpacing: string;
  codeFamily: string;
  fontFeatures: string;
}

export interface ThemeSurfaces {
  borderRadiusCard: string;
  borderRadiusButton: string;
  borderRadiusBadge: string;
  borderStyle: string;
  shadowModel: "none" | "subtle" | "vignette";
  texture: string;
}

export interface ThemeSpacing {
  cardPadding: string;
  itemGap: string;
  sectionGap: string;
}

export interface ThemeMotion {
  transitionDuration: string;
  transitionEasing: string;
}

export interface Theme {
  id: string;
  name: string;
  description: string;
  heritage: string;
  unlockCondition: string;
  unlockMessage: string;
  unlockHint: string;
  unlockProgressLabel: string;
  hidden: boolean;
  palette: ThemePalette;
  typography: ThemeTypography;
  surfaces: ThemeSurfaces;
  spacing: ThemeSpacing;
  motion: ThemeMotion;
}

// ---------------------------------------------------------------------------
// Shared defaults
// ---------------------------------------------------------------------------

const defaultTypography: ThemeTypography = {
  headingFamily: '"Inter Variable", ui-sans-serif, system-ui, sans-serif',
  headingWeight: "600",
  headingLetterSpacing: "0",
  bodyFamily: '"Inter Variable", ui-sans-serif, system-ui, sans-serif',
  bodySize: "14px",
  bodyLineHeight: "1.5",
  labelCase: "uppercase",
  labelLetterSpacing: "0.08em",
  codeFamily: '"JetBrains Mono Variable", ui-monospace, monospace',
  fontFeatures: "",
};

const defaultSurfaces: ThemeSurfaces = {
  borderRadiusCard: "8px",
  borderRadiusButton: "6px",
  borderRadiusBadge: "9999px",
  borderStyle: "1px solid",
  shadowModel: "subtle",
  texture: "none",
};

const defaultSpacing: ThemeSpacing = {
  cardPadding: "16px",
  itemGap: "12px",
  sectionGap: "24px",
};

const defaultMotion: ThemeMotion = {
  transitionDuration: "200ms",
  transitionEasing: "ease-out",
};

// ---------------------------------------------------------------------------
// Theme definitions
// ---------------------------------------------------------------------------

const conductorClassic: Theme = {
  id: "conductor-classic",
  name: "Conductor Classic",
  description: "Deep blue + brass — the default Conductor identity",
  heritage: "British Railways corporate livery + Pullman luxury coaches",
  unlockCondition: "always",
  unlockMessage: "The original.",
  unlockHint: "The original.",
  unlockProgressLabel: "",
  hidden: false,
  palette: {
    gray50: "#0A1628", gray100: "#162440", gray200: "#232D42", gray300: "#2A3550",
    gray400: "#4A5568", gray500: "#7B8494", gray600: "#7B8494", gray700: "#9CA3B0",
    gray800: "#C8CDD5", gray900: "#D8DCE2", gray950: "#E8EBF0", white: "#0F1D32",
    accent: "#2B5EA7", accentGlow: "#EBC47F", accentBg: "#1C2D4F",
    statusGo: "#39B54A", statusCaution: "#FF9500", statusStop: "#D73020",
  },
  typography: { ...defaultTypography },
  surfaces: { ...defaultSurfaces },
  spacing: { ...defaultSpacing },
  motion: { ...defaultMotion },
};

const londonUnderground: Theme = {
  id: "london-underground",
  name: "London Underground",
  description: "The Tube — roundels, Johnston type, and line colours",
  heritage: "Harry Beck's tube map, Johnston typeface, the roundel",
  unlockCondition: "repos_registered >= 5",
  unlockMessage: "You're running a network now.",
  unlockHint: "Mind the gap.",
  unlockProgressLabel: "Repos registered",
  hidden: false,
  palette: {
    gray50: "#0A0E1A", gray100: "#1A2040", gray200: "#222B52", gray300: "#2A3360",
    gray400: "#505872", gray500: "#8890A0", gray600: "#8890A0", gray700: "#A0A8B8",
    gray800: "#D4D8E0", gray900: "#E0E4EC", gray950: "#F0F2F6", white: "#11162B",
    accent: "#E32017", accentGlow: "#F4E9C4", accentBg: "#2A1520",
    statusGo: "#00843D", statusCaution: "#FFD329", statusStop: "#E32017",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Overpass Variable", "Gill Sans", ui-sans-serif, sans-serif',
    headingWeight: "600",
    labelCase: "title-case",
    labelLetterSpacing: "0.04em",
  },
  surfaces: {
    ...defaultSurfaces,
    borderRadiusCard: "0px",
    borderRadiusButton: "9999px",
    texture: "ceramic-tile",
  },
  spacing: { ...defaultSpacing, sectionGap: "28px" },
  motion: { ...defaultMotion, transitionDuration: "250ms" },
};

const swissFederal: Theme = {
  id: "swiss-federal",
  name: "Swiss Federal Railways",
  description: "Helvetica precision — SBB/CFF/FFS",
  heritage: "Josef Muller-Brockmann's SBB design manual, the station clock",
  unlockCondition: "workflow_streak >= 10",
  unlockMessage: "Swiss precision.",
  unlockHint: "Precision in motion.",
  unlockProgressLabel: "Consecutive successful workflows",
  hidden: false,
  palette: {
    gray50: "#0D0D0D", gray100: "#1F1F1F", gray200: "#2A2A2A", gray300: "#383838",
    gray400: "#555555", gray500: "#999999", gray600: "#999999", gray700: "#BBBBBB",
    gray800: "#E8E8E8", gray900: "#F0F0F0", gray950: "#FAFAFA", white: "#141414",
    accent: "#EC0000", accentGlow: "#FFFFFF", accentBg: "#2A0A0A",
    statusGo: "#4CAF50", statusCaution: "#EC0000", statusStop: "#B71C1C",
  },
  typography: {
    ...defaultTypography,
    headingWeight: "700",
    headingLetterSpacing: "0",
    bodySize: "14px",
    labelCase: "uppercase",
    labelLetterSpacing: "0.12em",
    fontFeatures: "'tnum'",
  },
  surfaces: {
    borderRadiusCard: "0px",
    borderRadiusButton: "0px",
    borderRadiusBadge: "0px",
    borderStyle: "1px solid",
    shadowModel: "none",
    texture: "none",
  },
  spacing: { cardPadding: "20px", itemGap: "16px", sectionGap: "32px" },
  motion: { transitionDuration: "150ms", transitionEasing: "linear" },
};

const shinkansen: Theme = {
  id: "shinkansen",
  name: "Shinkansen",
  description: "High-speed elegance — JR bullet trains",
  heritage: "Japanese station signage, E5 Hayabusa, sakura season",
  unlockCondition: "prs_merged >= 50",
  unlockMessage: "High-speed operations.",
  unlockHint: "High-speed elegance.",
  unlockProgressLabel: "PRs merged",
  hidden: false,
  palette: {
    gray50: "#0B1015", gray100: "#1A2330", gray200: "#222E3E", gray300: "#2A3648",
    gray400: "#4A5568", gray500: "#7A8694", gray600: "#7A8694", gray700: "#9AA6B4",
    gray800: "#E0E4E8", gray900: "#ECF0F4", gray950: "#F8FAFC", white: "#121920",
    accent: "#00B5AD", accentGlow: "#FF6B9D", accentBg: "#0A2220",
    statusGo: "#00B5AD", statusCaution: "#FFB020", statusStop: "#E53935",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Inter Variable", "Noto Sans JP", ui-sans-serif, sans-serif',
    headingWeight: "600",
    bodySize: "14px",
    labelCase: "normal",
    labelLetterSpacing: "0.04em",
  },
  surfaces: {
    ...defaultSurfaces,
    borderRadiusCard: "12px",
    borderRadiusButton: "8px",
    texture: "brushed-metal",
  },
  spacing: { cardPadding: "14px", itemGap: "10px", sectionGap: "20px" },
  motion: { transitionDuration: "120ms", transitionEasing: "ease-out" },
};

const platform934: Theme = {
  id: "platform-9-three-quarters",
  name: "Platform 9\u00BE",
  description: "Something magical this way comes...",
  heritage: "Hogwarts Express, King's Cross Station, magical Victorian railway",
  unlockCondition: "hidden",
  unlockMessage: "You found it.",
  unlockHint: "Something magical this way comes...",
  unlockProgressLabel: "",
  hidden: true,
  palette: {
    gray50: "#1A0A0A", gray100: "#2E1A1A", gray200: "#3A2222", gray300: "#4A2E2E",
    gray400: "#6B5445", gray500: "#9C8672", gray600: "#9C8672", gray700: "#B8A08A",
    gray800: "#E8D5C0", gray900: "#F0E4D4", gray950: "#FAF4EC", white: "#231212",
    accent: "#C9A84C", accentGlow: "#F5D98E", accentBg: "#3A2A18",
    statusGo: "#2D6A4F", statusCaution: "#C9A84C", statusStop: "#7C0A02",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Playfair Display Variable", "Crimson Pro", Georgia, serif',
    headingWeight: "700",
    bodyFamily: '"Inter Variable", ui-sans-serif, system-ui, sans-serif',
    labelCase: "title-case",
    labelLetterSpacing: "0.04em",
  },
  surfaces: {
    borderRadiusCard: "4px",
    borderRadiusButton: "4px",
    borderRadiusBadge: "9999px",
    borderStyle: "2px double",
    shadowModel: "vignette",
    texture: "parchment",
  },
  spacing: { cardPadding: "24px", itemGap: "12px", sectionGap: "24px" },
  motion: { transitionDuration: "300ms", transitionEasing: "ease-in-out" },
};

const orientExpress: Theme = {
  id: "orient-express",
  name: "Orient Express",
  description: "Art Deco luxury — golden age of rail travel",
  heritage: "Lalique glass, marquetry wood panels, brass fittings",
  unlockCondition: "usage_years >= 1",
  unlockMessage: "A seasoned traveler.",
  unlockHint: "A journey through time.",
  unlockProgressLabel: "Years of use",
  hidden: false,
  palette: {
    gray50: "#0C1A15", gray100: "#1B3A30", gray200: "#244A3D", gray300: "#2E5A4A",
    gray400: "#6B6050", gray500: "#A09480", gray600: "#A09480", gray700: "#B8AC98",
    gray800: "#E8DCC8", gray900: "#F0E8D8", gray950: "#FAF6F0", white: "#122620",
    accent: "#D4A843", accentGlow: "#F0D68A", accentBg: "#2A2818",
    statusGo: "#4CAF50", statusCaution: "#D4A843", statusStop: "#C0392B",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Josefin Sans", "Poiret One", ui-sans-serif, sans-serif',
    headingWeight: "300",
    headingLetterSpacing: "0.06em",
    labelCase: "uppercase",
    labelLetterSpacing: "0.2em",
  },
  surfaces: {
    borderRadiusCard: "2px",
    borderRadiusButton: "2px",
    borderRadiusBadge: "2px",
    borderStyle: "1px solid",
    shadowModel: "vignette",
    texture: "wood-marquetry",
  },
  spacing: { cardPadding: "24px", itemGap: "16px", sectionGap: "40px" },
  motion: { transitionDuration: "350ms", transitionEasing: "ease-in-out" },
};

const nycSubway: Theme = {
  id: "nyc-subway",
  name: "NYC Subway",
  description: "Vignelli brutalism — the city that never sleeps",
  heritage: "Massimo Vignelli's 1972 subway map, Helvetica signage",
  unlockCondition: "parallel_agents >= 10",
  unlockMessage: "Running a 24/7 operation.",
  unlockHint: "The city that never sleeps.",
  unlockProgressLabel: "Parallel agent sessions",
  hidden: false,
  palette: {
    gray50: "#0A0A0A", gray100: "#1E1E1E", gray200: "#282828", gray300: "#363636",
    gray400: "#585858", gray500: "#909090", gray600: "#909090", gray700: "#B0B0B0",
    gray800: "#EAEAEA", gray900: "#F2F2F2", gray950: "#FAFAFA", white: "#141414",
    accent: "#FCCC0A", accentGlow: "#FCCC0A", accentBg: "#2A2808",
    statusGo: "#00933C", statusCaution: "#FCCC0A", statusStop: "#EE352E",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Inter Variable", "Helvetica Neue", "Archivo Black", sans-serif',
    headingWeight: "800",
    headingLetterSpacing: "-0.01em",
    bodySize: "14px",
    bodyLineHeight: "1.3",
    labelCase: "uppercase",
    labelLetterSpacing: "-0.01em",
  },
  surfaces: {
    borderRadiusCard: "0px",
    borderRadiusButton: "0px",
    borderRadiusBadge: "0px",
    borderStyle: "2px solid",
    shadowModel: "none",
    texture: "subway-tile",
  },
  spacing: { cardPadding: "12px", itemGap: "8px", sectionGap: "24px" },
  motion: { transitionDuration: "100ms", transitionEasing: "ease-out" },
};

const transSiberian: Theme = {
  id: "trans-siberian",
  name: "Trans-Siberian",
  description: "The longest journey — birch forests and endurance",
  heritage: "World's longest railway, Russian railway heritage",
  unlockCondition: "workflow_steps >= 20",
  unlockMessage: "The longest journey.",
  unlockHint: "The longest journey begins.",
  unlockProgressLabel: "Steps in a single workflow",
  hidden: false,
  palette: {
    gray50: "#070D08", gray100: "#162218", gray200: "#1E2E20", gray300: "#263828",
    gray400: "#566058", gray500: "#8A9A8C", gray600: "#8A9A8C", gray700: "#A4B4A6",
    gray800: "#D4DDD6", gray900: "#E4EDE6", gray950: "#F4FAF4", white: "#0E1A10",
    accent: "#C0392B", accentGlow: "#E8D5B0", accentBg: "#2A1810",
    statusGo: "#4CAF50", statusCaution: "#E8D5B0", statusStop: "#C0392B",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Raleway Variable", "Jost", ui-sans-serif, sans-serif',
    headingWeight: "400",
    bodySize: "15px",
    bodyLineHeight: "1.7",
    labelCase: "title-case",
    labelLetterSpacing: "0.04em",
  },
  surfaces: {
    ...defaultSurfaces,
    borderRadiusCard: "6px",
    texture: "birch-grain",
  },
  spacing: { cardPadding: "24px", itemGap: "20px", sectionGap: "48px" },
  motion: { transitionDuration: "300ms", transitionEasing: "ease-in-out" },
};

const pullmanClass: Theme = {
  id: "pullman-class",
  name: "Pullman Class",
  description: "First class, always — gilded age craftsmanship",
  heritage: "George Pullman's luxury sleeping cars, community contribution",
  unlockCondition: "oss_contributor",
  unlockMessage: "First class, always.",
  unlockHint: "For those who contribute.",
  unlockProgressLabel: "OSS contributions",
  hidden: false,
  palette: {
    gray50: "#120C06", gray100: "#28200E", gray200: "#342A16", gray300: "#40341E",
    gray400: "#6B5C3E", gray500: "#A08A60", gray600: "#A08A60", gray700: "#B8A278",
    gray800: "#EBC47F", gray900: "#F0D8A0", gray950: "#FAF0D8", white: "#1C1408",
    accent: "#CD853F", accentGlow: "#F5E1A4", accentBg: "#342A16",
    statusGo: "#6B8E23", statusCaution: "#CD853F", statusStop: "#A0522D",
  },
  typography: {
    ...defaultTypography,
    headingFamily: '"Libre Baskerville Variable", "Georgia", serif',
    headingWeight: "700",
    bodyFamily: '"Libre Baskerville Variable", "Georgia", serif',
    bodySize: "15px",
    labelCase: "uppercase",
    labelLetterSpacing: "0.08em",
    fontFeatures: "'onum', 'smcp'",
  },
  surfaces: {
    borderRadiusCard: "3px",
    borderRadiusButton: "3px",
    borderRadiusBadge: "9999px",
    borderStyle: "1px solid",
    shadowModel: "subtle",
    texture: "mahogany-grain",
  },
  spacing: { cardPadding: "20px", itemGap: "14px", sectionGap: "28px" },
  motion: { transitionDuration: "250ms", transitionEasing: "ease-in-out" },
};

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

export const themes: Theme[] = [
  conductorClassic, londonUnderground, swissFederal, shinkansen,
  platform934, orientExpress, nycSubway, transSiberian, pullmanClass,
];

export const defaultTheme = conductorClassic;

export function getThemeById(id: string): Theme | undefined {
  return themes.find((t) => t.id === id);
}
