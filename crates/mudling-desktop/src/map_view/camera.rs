//! Where the map is looked at from: squares to pixels and back.

/// Pixels from one square to the next at zoom 1, as the canvas draws them.
pub const SQUARE: f32 = 46.;
pub const MIN_ZOOM: f32 = 0.15;
pub const MAX_ZOOM: f32 = 2.5;

/// A rectangle of squares. y grows north, as on the grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub west: f32,
    pub east: f32,
    pub south: f32,
    pub north: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// The square at the middle of the view.
    pub center: (f32, f32),
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            center: (0., 0.),
            zoom: 1.,
        }
    }
}

impl Camera {
    pub fn square_size(&self) -> f32 {
        SQUARE * self.zoom
    }

    /// Where a square's middle falls in a view of `size` pixels.
    pub fn to_view(self, (x, y): (f32, f32), (width, height): (f32, f32)) -> (f32, f32) {
        let square = self.square_size();
        (
            width / 2. + (x - self.center.0) * square,
            height / 2. - (y - self.center.1) * square,
        )
    }

    /// The fractional square under a point of the view.
    pub fn to_grid(self, (x, y): (f32, f32), (width, height): (f32, f32)) -> (f32, f32) {
        let square = self.square_size();
        (
            self.center.0 + (x - width / 2.) / square,
            self.center.1 - (y - height / 2.) / square,
        )
    }

    /// The squares a view of `size` pixels shows.
    pub fn region(&self, size: (f32, f32)) -> Region {
        let (west, north) = self.to_grid((0., 0.), size);
        let (east, south) = self.to_grid(size, size);
        Region {
            west,
            east,
            south,
            north,
        }
    }

    pub fn pan_pixels(&mut self, (dx, dy): (f32, f32)) {
        let square = self.square_size();
        self.center.0 -= dx / square;
        self.center.1 += dy / square;
    }

    /// Zooms by `factor`, keeping the middle square where it is.
    pub fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// Zooms by `factor`, keeping the square under `anchor` where it is.
    pub fn zoom_at(&mut self, factor: f32, anchor: (f32, f32), size: (f32, f32)) {
        let held = self.to_grid(anchor, size);
        self.zoom_by(factor);
        let moved = self.to_grid(anchor, size);
        self.center.0 += held.0 - moved.0;
        self.center.1 += held.1 - moved.1;
    }

    /// Frames a region of squares in a view of `size` pixels.
    pub fn fit(&mut self, region: Region, size: (f32, f32)) {
        let wide = (region.east - region.west + 1.).max(1.);
        let tall = (region.north - region.south + 1.).max(1.);
        let zoom = (size.0 / wide).min(size.1 / tall) / SQUARE;
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.center = (
            (region.west + region.east) / 2.,
            (region.south + region.north) / 2.,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f32, f32) = (400., 300.);

    #[test]
    fn north_is_up_and_a_point_round_trips() {
        let camera = Camera {
            center: (10., 20.),
            zoom: 0.5,
        };
        assert_eq!(camera.to_view((10., 20.), VIEW), (200., 150.));
        assert!(camera.to_view((10., 21.), VIEW).1 < 150.);
        assert_eq!(
            camera.to_grid(camera.to_view((3., 4.), VIEW), VIEW),
            (3., 4.)
        );
    }

    #[test]
    fn zooming_holds_the_square_under_the_pointer() {
        let mut camera = Camera::default();
        let anchor = (320., 40.);
        let under = camera.to_grid(anchor, VIEW);
        camera.zoom_at(1.5, anchor, VIEW);
        let after = camera.to_grid(anchor, VIEW);
        assert!((under.0 - after.0).abs() < 1e-4);
        assert!((under.1 - after.1).abs() < 1e-4);
    }

    #[test]
    fn zoom_stays_in_range() {
        let mut camera = Camera::default();
        camera.zoom_at(100., (0., 0.), VIEW);
        assert_eq!(camera.zoom, MAX_ZOOM);
        camera.zoom_at(0.0001, (0., 0.), VIEW);
        assert_eq!(camera.zoom, MIN_ZOOM);
    }
}
