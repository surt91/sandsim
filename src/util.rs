//! Small numeric helpers: random numbers, Gaussian filter, rectangles.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    /// splitmix64
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// standard normal (Box-Muller)
    pub fn normal(&mut self) -> f64 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// White noise field n×n, Gaussian filtered with sigma [px].
pub fn noise_field(n: usize, sigma: f64, seed: u64) -> Vec<f64> {
    let mut rng = Rng::new(seed);
    let mut a: Vec<f64> = (0..n * n).map(|_| rng.normal()).collect();
    gaussian_filter(&mut a, n, sigma);
    a
}

/// Separable Gaussian filter of an n×n field (reflecting border, truncated at 4 sigma).
pub fn gaussian_filter(a: &mut [f64], n: usize, sigma: f64) {
    if sigma <= 0.0 {
        return;
    }
    let r = (4.0 * sigma + 0.5) as isize;
    let mut k: Vec<f64> = (-r..=r).map(|x| (-0.5 * (x as f64 / sigma).powi(2)).exp()).collect();
    let s: f64 = k.iter().sum();
    k.iter_mut().for_each(|w| *w /= s);
    let refl = |i: isize| -> usize {
        let m = n as isize;
        let mut i = i;
        loop {
            if i < 0 {
                i = -i - 1;
            } else if i >= m {
                i = 2 * m - i - 1;
            } else {
                return i as usize;
            }
        }
    };
    let mut line = vec![0.0; n];
    for i in 0..n {
        for (j, l) in line.iter_mut().enumerate() {
            *l = (-r..=r).map(|d| k[(d + r) as usize] * a[i * n + refl(j as isize + d)]).sum();
        }
        a[i * n..(i + 1) * n].copy_from_slice(&line);
    }
    for j in 0..n {
        for (i, l) in line.iter_mut().enumerate() {
            *l = (-r..=r).map(|d| k[(d + r) as usize] * a[refl(i as isize + d) * n + j]).sum();
        }
        for i in 0..n {
            a[i * n + j] = line[i];
        }
    }
}

/// Half-open pixel rectangle [i0, i1) × [j0, j1) (rows, columns).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub i0: usize,
    pub i1: usize,
    pub j0: usize,
    pub j1: usize,
}

impl Rect {
    pub fn empty() -> Self {
        Self { i0: usize::MAX, i1: 0, j0: usize::MAX, j1: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.i0 >= self.i1 || self.j0 >= self.j1
    }

    pub fn union(&mut self, o: Rect) {
        if o.is_empty() {
            return;
        }
        self.i0 = self.i0.min(o.i0);
        self.i1 = self.i1.max(o.i1);
        self.j0 = self.j0.min(o.j0);
        self.j1 = self.j1.max(o.j1);
    }

    pub fn grow(&self, m: usize, n: usize) -> Rect {
        if self.is_empty() {
            return *self;
        }
        Rect {
            i0: self.i0.saturating_sub(m),
            i1: (self.i1 + m).min(n),
            j0: self.j0.saturating_sub(m),
            j1: (self.j1 + m).min(n),
        }
    }
}
