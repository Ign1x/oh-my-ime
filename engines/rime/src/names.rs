//! Schema names as readers of simplified Chinese expect them. rime-data names its
//! schemas in traditional characters (小鶴雙拼); outside the locales of traditional
//! Chinese (Taiwan, Hong Kong, Macau) the platform shows them in simplified ones
//! (小鹤双拼): in the input method list, the tray, the switcher and the settings.

/// The traditional characters of the names of rime-data's schemas, with their
/// simplified forms.
const SIMPLIFIED: [(char, char); 41] = [
    ('萬', '万'),
    ('數', '数'),
    ('號', '号'),
    ('組', '组'),
    ('臺', '台'),
    ('灣', '湾'),
    ('體', '体'),
    ('倉', '仓'),
    ('頡', '颉'),
    ('宮', '宫'),
    ('禪', '禅'),
    ('聲', '声'),
    ('調', '调'),
    ('輔', '辅'),
    ('碼', '码'),
    ('韻', '韵'),
    ('並', '并'),
    ('擊', '击'),
    ('動', '动'),
    ('態', '态'),
    ('雙', '双'),
    ('鶴', '鹤'),
    ('軟', '软'),
    ('廣', '广'),
    ('東', '东'),
    ('話', '话'),
    ('雲', '云'),
    ('龍', '龙'),
    ('國', '国'),
    ('際', '际'),
    ('標', '标'),
    ('粵', '粤'),
    ('語', '语'),
    ('無', '无'),
    ('蘇', '苏'),
    ('吳', '吴'),
    ('記', '记'),
    ('筆', '笔'),
    ('畫', '画'),
    ('簡', '简'),
    ('魯', '鲁'),
];

/// Whether the user reads traditional Chinese, after the locale.
fn traditional() -> bool {
    let locale = omime_proto::i18n::normalize(&omime_proto::i18n::locale());
    locale.starts_with("zh-")
        && locale
            .split('-')
            .any(|part| matches!(part, "tw" | "hk" | "mo" | "hant"))
}

/// The name of a schema to show.
pub fn display(name: &str) -> String {
    if traditional() {
        name.to_owned()
    } else {
        simplified(name)
    }
}

fn simplified(name: &str) -> String {
    name.chars()
        .map(|c| {
            SIMPLIFIED
                .iter()
                .find(|(traditional, _)| *traditional == c)
                .map_or(c, |&(_, simplified)| simplified)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_rime_data_read_in_simplified_characters() {
        assert_eq!(simplified("小鶴雙拼"), "小鹤双拼");
        assert_eq!(simplified("朙月拼音·語句流"), "朙月拼音·语句流");
        assert_eq!(simplified("倉頡五代"), "仓颉五代");
        assert_eq!(simplified("地球拼音（數字標調）"), "地球拼音（数字标调）");
        assert_eq!(simplified("五笔86"), "五笔86");
        assert_eq!(simplified("Emoji"), "Emoji");
    }
}
