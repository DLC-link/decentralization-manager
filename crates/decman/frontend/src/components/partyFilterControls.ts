import type { Dispatch, SetStateAction } from "react";

/**
 * The parties filter: the prefix field, its search, and the hidden-parties
 * toggle. The large-screen search bar and the parties tab both show them.
 */
export interface PartyFilterControls {
  partyFilter: string;
  setPartyFilter: (prefix: string) => void;
  refreshParties: (force?: boolean) => Promise<void>;
  refreshingParties: boolean;
  showHidden: boolean;
  setShowHidden: Dispatch<SetStateAction<boolean>>;
  hiddenCount: number;
}
