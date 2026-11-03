import copy
import json
import os
import unittest

import scorecard_diff as sd

SAMPLE = {
    "story": "S12", "tag": "default", "name": "route", "passed": True,
    "effort": {"actions": 10, "view_switches": 2, "dialogs_opened": 0,
               "klm": {"seconds": 20.0}, "wait_seconds": 0.0},
    "per_item": None,
    "rework": {"undos": 0, "round_trip": {"restores_start": True, "restores_end": True}},
    "late_changes": [{"name": "swap", "passed": True, "cost": 0.1, "klm_ratio": 0.1, "lost": []}],
    "surfaces": {"hidden_share": 0.1},
    "feedback": {"time_to_hear": {"actions": 5}},
    "robustness": {"underruns": 0},
}


class Diff(unittest.TestCase):
    def test_added_click_shows_up(self):
        new = copy.deepcopy(SAMPLE)
        new["effort"]["actions"] = 11
        out = sd.render([SAMPLE], [new])
        self.assertTrue(out.startswith(sd.MARKER))
        self.assertIn("| S12 default: route | actions | 10 | 11 |", out)

    def test_noise_is_ignored(self):
        new = copy.deepcopy(SAMPLE)
        new["effort"]["klm"]["seconds"] = 20.2
        new["late_changes"][0]["cost"] = 0.11
        self.assertIn("No route changed", sd.render([SAMPLE], [new]))

    def test_late_change_and_failure(self):
        new = copy.deepcopy(SAMPLE)
        new["passed"] = False
        new["late_changes"][0]["cost"] = 0.6
        out = sd.render([SAMPLE], [new])
        self.assertIn("| passes | yes | NO |", out)
        self.assertIn("late change 'swap' cost | 0.1 | 0.6", out)

    def test_no_baseline_and_new_route(self):
        self.assertIn("No baseline", sd.render(None, [SAMPLE]))
        other = copy.deepcopy(SAMPLE)
        other["story"] = "S13"
        self.assertIn("| S13 default: route | route | - | new |", sd.render([SAMPLE], [SAMPLE, other]))

    def test_an_older_baseline_layout_does_not_crash(self):
        old = {"story": "S12", "tag": "default", "name": "route", "passed": True,
               "effort": {"actions": 10}}
        out = sd.render([old], [SAMPLE])
        self.assertTrue(out.startswith(sd.MARKER))

    def test_real_scorecard_against_itself(self):
        path = os.environ.get("SCORECARD")
        if path:
            data = json.load(open(path))
            self.assertIn("No route changed", sd.render(data, data))


if __name__ == "__main__":
    unittest.main()
