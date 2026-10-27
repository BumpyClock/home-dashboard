import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("analyzer", Path(__file__).with_name("analyze-frame-trace.py"))
analyzer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analyzer)


def event(name, at, op, **fields):
    return {"type": "event", "name": name, "at_us": at, "op_id": op, "fields": fields}


class AnalyzeTrace(unittest.TestCase):
    def test_boundaries_failures_and_long_animation(self):
        data = [{"type": "session", "schema": 1},
                event("render_begin", 100, 1), event("render_end", 900, 1, success=False),
                event("photo_transition_start", 200, 2, surface="home"),
                event("photo_transition_end", 5_000_200, 2, surface="home"),
                event("photo_transition_start", 300, 3, surface="screensaver"),
                event("photo_transition_cancel", 400, 3),
                {"type": "summary", "dropped": 2, "truncated_fields": 1}]
        result = analyzer.analyze(data)
        self.assertEqual(result["stage_durations"]["render"]["max_us"], 800)
        self.assertEqual(result["stage_durations"]["render"]["failed"], 1)
        self.assertEqual(result["animation_intervals"][0]["duration_us"], 5_000_000)
        self.assertEqual(result["animation_intervals"][1]["outcome"], "cancel")
        self.assertEqual((result["dropped"], result["truncated_fields"]), (2, 1))

    def test_missing_and_repeated_spans(self):
        data = [{"type": "session", "schema": 1}, event("swap_end", 1, 1),
                event("swap_begin", 2, 2), event("swap_begin", 3, 2),
                event("swap_end", 8, 2), event("swap_end", 9, None)]
        result = analyzer.analyze(data)
        self.assertEqual(result["stage_durations"]["swap"]["max_us"], 5)
        self.assertEqual(result["incomplete_spans"], {"swap:missing_begin": 1, "swap:missing_end": 1, "swap:missing_id": 1})
        self.assertTrue(result["summary_missing"])
        self.assertIsNone(result["dropped"])

    def test_photo_durations_and_overlapping_frame_gaps(self):
        data = [{"type": "session", "schema": 1, "render_hooks_compiled": True},
                event("photo.decode", 0, 9, kind="thumb", elapsed_us=50),
                event("photo.decode", 1, 10, kind="thumb", elapsed_us=150),
                event("photo.decode", 2, 11, kind="full", elapsed_us=300),
                event("photo_transition_start", 100, 12, surface="home"),
                event("render_begin", 100, 1), event("render_end", 101, 1),
                event("render_begin", 500_100, 2), event("render_end", 500_101, 2),
                event("photo_transition_end", 500_100, 12),
                event("render_begin", 600_000, 3), event("render_end", 600_001, 3),
                {"type": "summary", "dropped": 0, "truncated_fields": 0}]
        result = analyzer.analyze(data)
        grouped = {(x["event"], x["kind"]): x for x in result["photo_durations"]}
        self.assertEqual(grouped[("photo.decode", "thumb")]["total_us"], 200)
        self.assertEqual(grouped[("photo.decode", "thumb")]["median_us"], 100)
        self.assertEqual(grouped[("photo.decode", "thumb")]["p95_us"], 150)
        self.assertEqual(grouped[("photo.decode", "full")]["count"], 1)
        self.assertEqual(result["animation_frame_gaps"][0]["duration_us"], 500_000)
        self.assertEqual(len(result["animation_frame_gaps"]), 1)
        self.assertFalse(result["animation_frame_gaps_incomplete"])

    def test_cancelled_transition_excludes_later_idle_gaps(self):
        data = [{"type": "session", "schema": 1, "render_hooks_compiled": True},
                event("photo_transition_start", 100, 1, surface="home"),
                event("render_begin", 100, 2), event("render_end", 101, 2),
                event("photo_transition_cancel", 200, 1),
                event("render_begin", 200, 3), event("render_end", 201, 3),
                event("render_begin", 1_000_000, 4), event("render_end", 1_000_001, 4),
                {"type": "summary", "dropped": 0, "truncated_fields": 0}]
        result = analyzer.analyze(data)
        self.assertEqual(result["incomplete_spans"], {})
        self.assertEqual(result["animation_intervals"][0]["outcome"], "cancel")
        self.assertEqual([gap["duration_us"] for gap in result["animation_frame_gaps"]], [100])
        self.assertFalse(result["animation_frame_gaps_incomplete"])

    def test_incomplete_animation_includes_gaps_and_flags_loss(self):
        data = [{"type": "session", "schema": 1},
                event("photo_transition_start", 10, 1, surface="home"),
                event("render_begin", 20, 2), event("render_begin", 300_000, 3),
                {"type": "summary", "dropped": 1, "truncated_fields": 0}]
        result = analyzer.analyze(data)
        self.assertEqual(result["animation_intervals"][0]["outcome"], "incomplete")
        self.assertEqual(result["animation_frame_gaps"][0]["duration_us"], 299_980)
        self.assertTrue(result["animation_frame_gaps_incomplete"])

    def test_unsupported_session_rejected(self):
        with self.assertRaises(ValueError):
            analyzer.analyze([{"type": "session", "schema": 2}])


if __name__ == "__main__":
    unittest.main()
