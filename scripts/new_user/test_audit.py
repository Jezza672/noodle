#!/usr/bin/env python3
"""Tests for audit.py: command lines it must accept and refuse.
    python3 scripts/new_user/test_audit.py
"""
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import audit  # noqa: E402

CTL = "/x/agent/ctl.sh"
B = "bash " + CTL + " "

ACCEPT = [
    B + "look",
    B + 'note "a #1; b | c && d"; ' + B + "look",
    B + "look; " + B + "tree | head -60",
    B + "tree | grep -i -E \"formant|F minor\"",
    B + "tree | grep -A40 -i view | head -60",
    B + "tree | grep -n -e a -e b",
    B + "tree | tail -n +81",
    B + "look; sleep 20",
    B + "note 'it'\"'\"'s #fine'",
    B + 'note "a\nb"',
]
REFUSE = [
    "cat x",
    B + "look; cat /etc/passwd",
    B + "look $(id)",
    B + "look `id`",
    B + "look\ncat /etc/passwd",
    B + "look > /tmp/x",
    B + "look | sh",
    B + "look & cat x",
    B + "look || cat x",
    B + "note a#b ; head /etc/passwd",  # shlex would call a#b a comment
    B + "note #\ncat /etc/passwd",  # so would a bare #
    B + "note \\'\nhead /etc/passwd\n: \\'",  # an escaped quote is no quote
    B + 'note "a\\"b"',
    B + "tree | grep -epat /etc/passwd",
    B + "tree | grep --regexp=x /etc/passwd",
    B + "tree | grep -ri secret",
    B + "tree | grep -Rn x",
    B + "tree | grep -drecurse x",
    B + "tree | grep -f /etc/passwd",
    B + "tree | grep -i x /etc/passwd",
    B + "tree | head -5 | cat /etc/passwd",
    B + "tree | cat /etc/passwd",
]


class OnlyCtl(unittest.TestCase):
    def test_accepts(self):
        for c in ACCEPT:
            self.assertTrue(audit.only_ctl(c, CTL), c)

    def test_refuses(self):
        for c in REFUSE:
            self.assertFalse(audit.only_ctl(c, CTL), c)


class Denied(unittest.TestCase):
    def events(self, result):
        return [
            json.dumps({"message": {"content": [{"type": "tool_use", "id": "1", "name": "Bash", "input": {"command": "cat /etc/passwd; echo Permission to use"}}]}}),
            json.dumps({"message": {"content": [{"type": "tool_result", "tool_use_id": "1", **result}]}}),
        ]

    def test_a_command_that_ran_is_not_excused_by_printing_the_phrase(self):
        out = audit.stray_calls(self.events({"content": "root:x Permission to use"}), Path("/x/agent"))
        self.assertFalse(out[0]["denied"])

    def test_a_refusal_is_denied(self):
        out = audit.stray_calls(
            self.events({"is_error": True, "content": "Permission to use Bash has been denied because"}),
            Path("/x/agent"),
        )
        self.assertTrue(out[0]["denied"])


if __name__ == "__main__":
    unittest.main()
