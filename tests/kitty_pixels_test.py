#!/usr/bin/env python3
"""Prove the screenshot oracle rejects the failures that text checks miss."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("driver", Path(__file__).with_name("kitty_e2e.py"))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
picture = driver.picture


class Pixels(unittest.TestCase):
    def frame(self, number=2, width=180, height=120):
        return driver.decode_png(picture.png(number, width, height))

    def test_correct_pictures_in_portrait_landscape_and_wide(self):
        for width, height in ((180, 120), (120, 180), (300, 100)):
            frame = self.frame(width=width, height=height)
            self.assertTrue(picture.match(frame, (0, 0, width, height), 2, (width * 20, height * 20))["ok"])

    def test_textured_picture_tolerates_thumbnail_rounding(self):
        frame = driver.decode_png(picture.png(2, 180, 120, texture=True))
        self.assertTrue(picture.match(frame, (0, 0, 180, 120), 2, (6000, 4000))["ok"])

    def test_wrong_image_is_rejected(self):
        self.assertFalse(picture.match(self.frame(number=3), (0, 0, 180, 120), 2, (180, 120))["ok"])

    def test_blank_or_literal_placeholder_glyphs_are_rejected(self):
        frame = self.frame()
        for pixels in (bytes(len(frame["pixels"])), bytes(c if i % 7 == 0 else 0 for i, c in enumerate(frame["pixels"]))):
            frame["pixels"] = pixels
            self.assertFalse(picture.match(frame, (0, 0, 180, 120), 2, (180, 120))["ok"])

    def test_stretched_or_transposed_picture_is_rejected(self):
        self.assertFalse(picture.match(self.frame(), (0, 0, 180, 120), 2, (120, 180))["ok"])
        self.assertFalse(picture.match(self.frame(), (0, 0, 180, 120), 2, (200, 120))["ok"])

    def test_wrong_spatial_layout_is_rejected(self):
        frame = self.frame()
        frame["pixels"] = frame["pixels"][::-1]
        self.assertFalse(picture.match(frame, (0, 0, 180, 120), 2, (180, 120))["ok"])

    def test_holes_between_samples_are_rejected(self):
        frame = self.frame()
        pixels = bytearray(frame["pixels"])
        for y in range(1, 119):
            for x in range(1, 179):
                if x % 6 != 3 or y % 4 != 2:
                    pixels[(y * 180 + x) * 3:(y * 180 + x) * 3 + 3] = b"\0\0\0"
        frame["pixels"] = pixels
        self.assertFalse(picture.match(frame, (0, 0, 180, 120), 2, (180, 120))["ok"])

    def test_tiny_image_in_large_placement_is_rejected(self):
        frame, tiny = self.frame(), self.frame(width=60, height=40)
        pixels = bytearray(len(frame["pixels"]))
        for y in range(40):
            pixels[y * 180 * 3:y * 180 * 3 + 180] = tiny["pixels"][y * 180:(y + 1) * 180]
        frame["pixels"] = pixels
        self.assertFalse(picture.match(frame, (0, 0, 180, 120), 2, (180, 120))["ok"])

    def test_outside_screen_is_rejected(self):
        self.assertFalse(picture.match(self.frame(), (0, 0, 180, 121), 2, (180, 120))["ok"])

    def test_combining_marks_do_not_move_picture_coordinates(self):
        cell = picture.PLACEHOLDER + "\u0305\u030d\u030e"
        row = "  " + cell * 3 + "   " + cell * 2
        self.assertEqual(picture.rectangles(row + "\n" + row, 0, 2, (10, 20)),
                         [(20, 0, 50, 40), (80, 0, 100, 40)])
        self.assertEqual(picture.rectangles("missing", 0, 1, (10, 20)), [])

    def test_demo_output_is_rejected(self):
        clean = "TITLE name\nCOLORSCHEME\n"
        self.assertTrue(driver.clean_preview(clean, "TITLE name"))
        for diagnostic in ("Sampled readability", "Measured image", "editor.go", "example diagnostic", "selected text",
                           "opacity 0.65", "contrast target 7.0", "arbitrary extra output"):
            self.assertFalse(driver.clean_preview(clean + diagnostic, "TITLE name"))
        self.assertFalse(driver.clean_preview("TITLE name\n", "TITLE name"))

    def test_every_swatch_must_be_visibly_painted(self):
        colours = [(i * 10, 50, 90) for i in range(16)]
        row = b"".join(bytes(c) * 4 for c in colours)
        frame = {"width": 64, "rows": 4, "channels": 3, "pixels": row * 4}
        self.assertTrue(picture.swatches(frame, 0, 0, colours, (1, 4)))
        for missing in range(16):
            pixels = bytearray(row * 4)
            for y in range(4):
                start = (y * 64 + missing * 4) * 3
                pixels[start:start + 12] = bytes(12)
            self.assertFalse(picture.swatches(frame | {"pixels": pixels}, 0, 0, colours, (1, 4)))


if __name__ == "__main__":
    unittest.main()
