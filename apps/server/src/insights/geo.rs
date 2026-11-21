use std::net::IpAddr;

use axum::http::HeaderMap;
use maxminddb::{Reader, geoip2};
use owlee_insights::clip;
use owlee_platform::GeoPrecision;

#[derive(Clone, Debug, Default)]
pub struct Place {
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
}

pub fn locate(
    reader: Option<&Reader<Vec<u8>>>,
    headers: &HeaderMap,
    proxy_hops: usize,
    ip: &str,
    precision: GeoPrecision,
) -> Place {
    if precision == GeoPrecision::Off {
        return Place::default();
    }
    let cloudflare = from_cloudflare(headers, proxy_hops);
    let database = from_database(reader, ip).unwrap_or_default();
    let mut place = Place {
        country: cloudflare.country.or(database.country),
        region: cloudflare.region.or(database.region),
        city: cloudflare.city.or(database.city),
    };
    if precision != GeoPrecision::City {
        place.city = None;
    }
    if precision == GeoPrecision::Country {
        place.region = None;
    }
    place
}

fn header(headers: &HeaderMap, name: &str, max: usize) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| clip(value.trim(), max))
        .filter(|value| !value.is_empty())
}

fn from_cloudflare(headers: &HeaderMap, proxy_hops: usize) -> Place {
    if proxy_hops < 2 {
        return Place::default();
    }
    let country = header(headers, "cf-ipcountry", 2)
        .filter(|value| {
            value
                .chars()
                .all(|character| character.is_ascii_alphabetic())
        })
        .map(|value| value.to_uppercase())
        .filter(|value| value != "XX");
    Place {
        region: country
            .as_ref()
            .and_then(|_| header(headers, "cf-region", 50)),
        city: country
            .as_ref()
            .and_then(|_| header(headers, "cf-ipcity", 50)),
        country,
    }
}

fn from_database(reader: Option<&Reader<Vec<u8>>>, ip: &str) -> Option<Place> {
    let ip: IpAddr = ip.parse().ok()?;
    let city = reader?
        .lookup(ip)
        .ok()?
        .decode::<geoip2::City>()
        .ok()
        .flatten()?;
    let named = |value: Option<&str>, max: usize| {
        value
            .map(|value| clip(value, max))
            .filter(|value| !value.is_empty())
    };
    Some(Place {
        country: named(city.country.iso_code, 2),
        region: named(
            city.subdivisions
                .first()
                .and_then(|item| item.names.english),
            50,
        ),
        city: named(city.city.names.english, 50),
    })
}

pub fn open_reader(path: &std::path::Path) -> Option<Reader<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Reader::from_source(bytes).ok(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "analytics geolocation database was not opened");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;
    use owlee_platform::GeoPrecision;

    use super::locate;

    fn cloudflare() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ipcountry", "nl".parse().unwrap());
        headers.insert("cf-region", "North Holland".parse().unwrap());
        headers.insert("cf-ipcity", "Amsterdam".parse().unwrap());
        headers
    }

    fn fields(precision: GeoPrecision) -> [Option<String>; 3] {
        let place = locate(None, &cloudflare(), 2, "203.0.113.7", precision);
        [place.country, place.region, place.city]
    }

    #[test]
    fn precision_drops_finer_fields() {
        let some = |value: &str| Some(value.to_owned());
        assert_eq!(
            fields(GeoPrecision::City),
            [some("NL"), some("North Holland"), some("Amsterdam")]
        );
        assert_eq!(
            fields(GeoPrecision::Region),
            [some("NL"), some("North Holland"), None]
        );
        assert_eq!(fields(GeoPrecision::Country), [some("NL"), None, None]);
        assert_eq!(fields(GeoPrecision::Off), [None, None, None]);
    }
}
