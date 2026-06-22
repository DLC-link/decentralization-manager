import { ToggleButton, ToggleButtonGroup, Tooltip } from "@mui/material";
import LightModeIcon from "@mui/icons-material/LightMode";
import DarkModeIcon from "@mui/icons-material/DarkMode";
import SettingsBrightnessIcon from "@mui/icons-material/SettingsBrightness";
import { useThemeMode } from "../contexts";

interface ThemeSwitcherProps {
  orientation?: "horizontal" | "vertical";
}

export const ThemeSwitcher = ({ orientation = "horizontal" }: ThemeSwitcherProps) => {
  const { mode, setMode } = useThemeMode();

  return (
    <ToggleButtonGroup
      value={mode}
      exclusive
      orientation={orientation}
      onChange={(_, newMode) => newMode && setMode(newMode)}
      size="small"
      sx={{
        backgroundColor: "background.paper",
        border: 1,
        borderColor: "divider",
        "& .MuiToggleButton-root": {
          border: "none",
          px: 1.5,
          "&.Mui-selected": {
            backgroundColor: "primary.main",
            color: "white",
            "&:hover": {
              backgroundColor: "primary.dark",
            },
          },
        },
      }}
    >
      <ToggleButton value="light">
        <Tooltip title="Light">
          <LightModeIcon fontSize="small" />
        </Tooltip>
      </ToggleButton>
      <ToggleButton value="auto">
        <Tooltip title="System">
          <SettingsBrightnessIcon fontSize="small" />
        </Tooltip>
      </ToggleButton>
      <ToggleButton value="dark">
        <Tooltip title="Dark">
          <DarkModeIcon fontSize="small" />
        </Tooltip>
      </ToggleButton>
    </ToggleButtonGroup>
  );
}
