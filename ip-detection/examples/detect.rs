use cabin_ip_detection::Detector;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let detector = Detector::builder().build()?;
    let info = detector.detect()?;
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
