use lumen_desktop::core::{Frame, Light, Point, Route, SamplingPlan, Shape, validate_lights};
fn main() {
    let x = 0.9f32;
    let next = f32::from_bits(x.to_bits() + 1);
    let light = Light {
        id: "tiny-imported-strip".into(), name: "Tiny imported strip".into(), zones: 1,
        route: Route::Mock,
        shape: Shape::Strip {
            points: vec![Point { x, y: 0.9 }, Point { x: next, y: 0.9 }],
            radius: 0.00001, reverse: false,
        },
    };
    validate_lights(std::slice::from_ref(&light)).unwrap();
    let mut frame = Frame { width: 160, height: 90, pixels: vec![[0, 255, 0]; 160 * 90] };
    frame.pixels[0] = [255, 0, 0];
    let colors = SamplingPlan::compile(&[light], 160, 90).unwrap().sample(&frame);
    println!("normalized x {x:?} vs {next:?}; converted x {} vs {}", x * 160.0, next * 160.0);
    println!("actual {colors:?}; expected near declared points: green [0,1,0]");
    assert_eq!(colors, vec![vec![[1.0, 0.0, 0.0]]]);
}
