import pytest

from deepstrike import (
    Tournament,
    LoopUntilDone,
    RoundReport,
    StopCondition,
)


def test_tournament_resolves_a_winner():
    t = Tournament(["a", "b", "c", "d"])
    r1 = t.start()
    assert r1.kind == "judge_round"
    assert r1.round == 1
    assert [(m.id, m.left, m.right) for m in r1.matches] == [(0, "a", "b"), (1, "c", "d")]

    r2 = t.feed_round(["a", "d"])
    assert r2.kind == "judge_round"
    assert r2.round == 2
    assert [(m.left, m.right) for m in r2.matches] == [("a", "d")]

    done = t.feed_round(["d"])
    assert done.kind == "done"
    assert done.winner == "d"
    assert done.rounds_used == 2
    assert t.is_done()


def test_tournament_bye_advances_odd_entrant():
    t = Tournament(["a", "b", "c"])
    r1 = t.start()
    assert [(m.left, m.right) for m in r1.matches] == [("a", "b")]
    r2 = t.feed_round(["a"])  # c got a bye
    assert [(m.left, m.right) for m in r2.matches] == [("a", "c")]
    assert t.feed_round(["c"]).winner == "c"


def test_tournament_single_entrant_immediate_done():
    done = Tournament(["solo"]).start()
    assert done.kind == "done"
    assert done.winner == "solo"
    assert done.rounds_used == 0


def test_tournament_empty_raises():
    with pytest.raises(ValueError):
        Tournament([])


def test_tournament_wrong_winner_count_raises():
    t = Tournament(["a", "b", "c", "d"])
    t.start()
    with pytest.raises(ValueError):
        t.feed_round(["a"])


def test_loop_stops_on_no_new_findings():
    loop = LoopUntilDone([StopCondition.no_new_findings()])
    assert loop.start().round == 1
    a = loop.feed(RoundReport(3, 0))
    assert a.kind == "spawn" and a.round == 2
    done = loop.feed(RoundReport(0, 9))
    assert done.kind == "done"
    assert done.rounds_used == 2
    assert done.reason == "no_new_findings"
    assert loop.is_done()


def test_loop_max_rounds_caps():
    loop = LoopUntilDone([StopCondition.max_rounds_at(2)])
    loop.start()
    assert loop.feed(RoundReport(1, 1)).kind == "spawn"
    done = loop.feed(RoundReport(1, 1))
    assert done.kind == "done"
    assert done.reason == "max_rounds"
    assert done.rounds_used == 2


def test_loop_first_condition_wins():
    loop = LoopUntilDone([StopCondition.no_new_findings(), StopCondition.no_errors()])
    loop.start()
    assert loop.feed(RoundReport(0, 0)).reason == "no_new_findings"


def test_loop_unknown_condition_raises():
    with pytest.raises(ValueError):
        LoopUntilDone([StopCondition("bogus")])
