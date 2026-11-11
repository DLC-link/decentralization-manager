import { FormControl, MenuItem, Select } from "@mui/material";

interface RelTimeSelectProps {
  /** The timeout in microseconds. Zero or unset shows one hour. */
  microseconds: number;
  onChange: (microseconds: number) => void;
}

/** The proposal timeout picker of a rel_time field. */
export const RelTimeSelect = ({ microseconds, onChange }: RelTimeSelectProps) => (
  <FormControl size="small" sx={{ width: 130 }}>
    <Select
      value={microseconds || 3600000000}
      onChange={(e) => onChange(Number(e.target.value))}
    >
      <MenuItem value={180000000}>3 min</MenuItem>
      <MenuItem value={600000000}>10 min</MenuItem>
      <MenuItem value={1800000000}>30 min</MenuItem>
      <MenuItem value={3600000000}>1 hour</MenuItem>
      <MenuItem value={7200000000}>2 hours</MenuItem>
      <MenuItem value={86400000000}>24 hours</MenuItem>
    </Select>
  </FormControl>
);
