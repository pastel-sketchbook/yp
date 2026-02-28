use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Config {
  pub theme_name: Option<String>,
  pub frame_mode: Option<String>,
}

impl Config {
  pub fn load() -> Self {
    if let Some(proj_dirs) = ProjectDirs::from("", "", "yp") {
      let config_file = proj_dirs.config_dir().join("prefs.toml");
      if let Ok(content) = std::fs::read_to_string(config_file)
        && let Ok(config) = toml::from_str(&content)
      {
        return config;
      }
    }
    Self::default()
  }

  pub fn save(&self) {
    if let Some(proj_dirs) = ProjectDirs::from("", "", "yp") {
      let config_dir = proj_dirs.config_dir();
      if std::fs::create_dir_all(config_dir).is_ok() {
        let config_file = config_dir.join("prefs.toml");
        if let Ok(content) = toml::to_string(self) {
          let _ = std::fs::write(config_file, content);
        }
      }
    }
  }
}
