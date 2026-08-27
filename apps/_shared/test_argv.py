import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from canonical_argv import (
    normalize_argparse_booleans,
    normalize_canonical_argv,
    parse_canonical_argv,
)


def test_normalizes_inline_values_boolean_false_and_delimiter():
    assert normalize_canonical_argv(
        ["--confirm=false", "--label=--urgent", "--", "--literal"],
        bool_flags={"confirm"},
    ) == ["--label", "--urgent", "--literal"]


def test_preserves_repeatable_occurrence_order():
    assert normalize_canonical_argv(
        ["--header=A: 1", "--header=B: 2"],
    ) == ["--header", "A: 1", "--header", "B: 2"]


def test_parser_preserves_option_shaped_positionals_after_delimiter():
    positionals, options = parse_canonical_argv(
        ["--label=--urgent", "--", "--label"],
        value_flags={"label"},
    )
    assert options == {"label": "--urgent"}
    assert positionals == ["--label"]


def test_argparse_boolean_normalization_preserves_inline_value_flags():
    assert normalize_argparse_booleans(
        ["--unread=false", "--query=--urgent", "--", "--unread=false"],
        bool_flags={"unread"},
    ) == ["--query=--urgent", "--", "--unread=false"]
