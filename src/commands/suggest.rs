use std::{borrow::Cow, ops::Deref, sync::Arc};

use color_eyre::eyre::{self, eyre, Context, OptionExt};
use geo::{Contains, Distance, Haversine, Length, Line, Point};
use serenity::{
    all::{
        Color, CommandInteraction, CommandOptionType, CreateButton, CreateCommandOption,
        CreateEmbed, CreateEmbedAuthor, EditMessage, ResolvedOption, ResolvedValue,
    },
    builder::CreateCommand,
};
use tracing::instrument;
use uom::{
    fmt::DisplayStyle,
    si::{
        length::{meter, Units},
        time::hour,
        velocity::mile_per_hour,
    },
};

use crate::{web_interface::upload_gpx::UploadForm, AppState};

pub fn create_command() -> CreateCommand {
    CreateCommand::new("suggest")
        .description("Suggest a hike that the gang can go on")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "alltrails_link",
                "Post a link to an AllTrails hike in Utah",
            )
            .required(true),
        )
}

struct ElevationPoint {
    point: Point,
    distance: f64,
    elevation: f64,
    extremum: bool,
    survived: bool,
}

#[derive(Debug)]
pub struct SuggestionCommand<'a> {
    pub suggestion_link: Cow<'a, str>,
}

impl<'a> SuggestionCommand<'a> {
    #[instrument]
    pub fn from_options(options: &[ResolvedOption<'a>]) -> Result<Self, eyre::Report> {
        match options.get(0).ok_or_eyre("No arguments were passed")? {
            ResolvedOption {
                value: ResolvedValue::String(suggestion_link),
                ..
            } => Ok(SuggestionCommand {
                suggestion_link: Cow::Borrowed(suggestion_link),
            }),
            _ => Err(eyre!("Option passed was not the right type")),
        }
    }

    #[instrument(skip(command, state))]
    pub async fn respond(
        mut self,
        command: &CommandInteraction,
        state: Arc<AppState>,
        author: String,
    ) -> Result<CreateEmbed, eyre::Report> {
        if !self
            .suggestion_link
            .starts_with("https://www.alltrails.com")
        {
            return Err(eyre!(
                "Trail suggestion was not from <https://www.alltrails.com>"
            ));
        }

        if self
            .suggestion_link
            .starts_with("https://www.alltrails.com/explore/")
        {
            self.suggestion_link = Cow::Owned(format!(
                "https://www.alltrails.com/{}",
                &self.suggestion_link["https://www.alltrails.com/explore/".len()..]
            ))
        }

        if !self
            .suggestion_link
            .starts_with("https://www.alltrails.com/trail/us/utah")
        {
            return Err(eyre!("Trail suggestion is not in Utah"));
        }

        let interaction = command.clone();

        tokio::spawn(async move {
            let http = state.http.load();
            let mut response = interaction.get_response(http.deref()).await.unwrap();
            response
                .edit(
                    http.deref(),
                    EditMessage::new().button(
                        CreateButton::new_link(format!(
                            "{}/hikea/upload_gpx/{}/{}",
                            state.config.load().hostname,
                            response.channel_id.get(),
                            response.id.get()
                        ))
                        .label("Upload AllTrails data for Trail"),
                    ),
                )
                .await
                .unwrap();
        });

        Ok(CreateEmbed::new()
            .color(Color::DARK_GREEN)
            .title("Trail suggestion!")
            .author(CreateEmbedAuthor::new(author))
            .description(
                "Someone suggested a trail! \
                        An admin will take your suggestion and \
                        fill it in with trail information shortly",
            )
            .url(self.suggestion_link))
    }
}

#[instrument(skip_all)]
pub fn embed_from_gpx(
    link: &str,
    short_units: Units,
    long_units: Units,
    avg_speed: f64,
    form: UploadForm,
) -> eyre::Result<CreateEmbed> {
    let utah_rect = geo::Rect::new(
        geo::coord! { x: -114.093, y: 42.017 },
        geo::coord! { x: -108.995, y: 36.933 },
    );

    let metadata = form
        .gpx_file
        .metadata
        .ok_or_eyre("GPX File has no metadata")?;

    if metadata
        .links
        .get(0)
        .ok_or_eyre("GPX File metadata has no links")?
        .href
        != "http://www.alltrails.com"
    {
        return Err(eyre!("GPX File did not originate from AllTrails"));
    }

    if !utah_rect.contains(
        &metadata
            .bounds
            .ok_or_eyre("GPX file did not have boundry metadata")?,
    ) {
        return Err(eyre!("Uploaded GPX trail is not in Utah"));
    }

    let track = form
        .gpx_file
        .tracks
        .get(0)
        .ok_or_eyre("GPX file contained no tracks")?;

    let line_string = track.multilinestring();
    let length = line_string.length::<Haversine>();
    let mut gains = 0.0;
    let mut losses = 0.0;
    let mut max_altitude = 0.0;
    let mut min_altitude = f64::MAX;
    let mut avg = (0.0, 0);
    for segment in &track.segments {
        for point in segment.points.iter() {
            let elevation = point
                .elevation
                .ok_or_eyre("Waypoint does not contain elevation data")?;
            avg.0 += elevation;
            avg.1 += 1;
            if max_altitude < elevation {
                max_altitude = elevation;
            }
            if min_altitude > elevation {
                min_altitude = elevation;
            }
        }
    }
    let elevation_points = vec![ElevationPoint {
        distance: 0.0,
        elevation: track
            .segments
            .get(0)
            .ok_or_eyre("GPX track has no segments")?
            .points
            .get(0)
            .ok_or_eyre("GPX segment has no points")?
            .elevation
            .ok_or_eyre("Waypoint does not have elevation data")?,
        extremum: true,
        survived: false,
        point: track
            .segments
            .get(0)
            .ok_or_eyre("GPX track has no segments")?
            .points
            .get(0)
            .ok_or_eyre("GPX segment has no points")?
            .point(),
    }];
    let mut elevation_points = track
        .segments
        .iter()
        .flat_map(|s| s.points.windows(2))
        .try_fold(
            (elevation_points, 0.0),
            |(mut points, mut distance), point| {
                distance += Haversine::distance(point[0].point(), point[1].point());
                points.push(ElevationPoint {
                    distance,
                    elevation: point[1]
                        .elevation
                        .ok_or_eyre("Waypoint does not have elevation data")?,
                    extremum: false,
                    survived: false,
                    point: point[1].point(),
                });
                Ok::<_, eyre::Report>((points, distance))
            },
        )?
        .0;
    approximate_elevation_points(&mut elevation_points)
        .wrap_err("Failed to approximate elevation points")?;
    elevation_points
        .last_mut()
        .ok_or_eyre("Finding elevation points yeilded no results")?
        .extremum = true;
    elevation_points
        .first_mut()
        .ok_or_eyre("Finding elevation points yeilded no results")?
        .extremum = true;
    find_maximum_extremum_between(0, elevation_points.len() - 1, &mut elevation_points)
        .wrap_err("Failed to find maximum extremum of elevation points")?;
    let mut prev_elevation_point = elevation_points
        .first()
        .ok_or_eyre("Finding elevation points yeilded no results")?;
    for elevation_point in elevation_points.iter().skip(1).filter(|e| e.extremum) {
        let diff = elevation_point.elevation - prev_elevation_point.elevation;
        if diff > 0.0 {
            gains += diff;
        } else {
            losses += diff.abs();
        }
        prev_elevation_point = elevation_point;
    }

    let travel_time = uom::si::f64::Length::new::<meter>(length)
        / uom::si::f64::Velocity::new::<mile_per_hour>(avg_speed);

    Ok(CreateEmbed::new()
        .color(Color::DARK_GREEN)
        .url(link)
        .title(form.title)
        .description(form.description)
        .field("Difficulty", form.difficulty, false)
        .field("Rating", form.rating, false)
        .field(
            "Approximate Time to Complete",
            format!(
                "{:.2}",
                travel_time.into_format_args(hour, DisplayStyle::Abbreviation)
            ),
            false,
        )
        .field(
            "Length",
            format_length(length, long_units).wrap_err("Failed to format length")?,
            false,
        )
        .field(
            "Uphill",
            format_length(gains, short_units).wrap_err("Failed to format length")?,
            true,
        )
        .field(
            "Downhill",
            format_length(losses, short_units).wrap_err("Failed to format length")?,
            true,
        )
        .field(
            "Avg. Elevation",
            format_length(avg.0 / avg.1 as f64, short_units).wrap_err("Failed to format length")?,
            false,
        )
        .field(
            "Minimum altitude",
            format_length(min_altitude, short_units).wrap_err("Failed to format length")?,
            true,
        )
        .field(
            "Maximum altitude",
            format_length(max_altitude, short_units).wrap_err("Failed to format length")?,
            true,
        )
        .image(form.image))
}

#[instrument]
fn format_length(length: f64, unit: uom::si::length::Units) -> eyre::Result<String> {
    let length = uom::si::f64::Length::new::<meter>(length);
    match unit {
        uom::si::length::Units::yottameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::zettameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::exameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::petameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::terameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::gigameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::megameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::kilometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::hectometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::decameter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::meter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::decimeter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::centimeter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::millimeter(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::micrometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::nanometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::picometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::femtometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::attometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::zeptometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::yoctometer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::angstrom(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::bohr_radius(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::atomic_unit_of_length(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::astronomical_unit(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::chain(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::fathom(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::fermi(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::foot(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::foot_survey(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::inch(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::light_year(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::microinch(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::micron(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::mil(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::mile(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::mile_survey(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::nautical_mile(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::parsec(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::pica_computer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::pica_printers(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::point_computer(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::point_printers(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::rod(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        uom::si::length::Units::yard(u) => Ok(format!(
            "{:.1}",
            length.into_format_args(u, DisplayStyle::Abbreviation)
        )),
        u => Err(eyre!("Conversion from unit `{:?}` is not implemented", u)),
    }
}

// Borrowed from OsmAnd: https://github.com/osmandapp/OsmAnd/blob/0026e71e1be4cd29fb904c5d0735f02cf80d88b6/OsmAnd-shared/src/commonMain/kotlin/net/osmand/shared/gpx/ElevationDiffsCalculator.kt#L20
// https://github.com/osmandapp/OsmAnd/blob/master/OsmAnd-shared/src/commonMain/kotlin/net/osmand/shared/gpx/ElevationApproximator.kt#L5

fn get_projection_dist(x: f64, y: f64, fromx: f64, fromy: f64, tox: f64, toy: f64) -> f64 {
    let m_dist = (fromx - tox) * (fromx - tox) + (fromy - toy) * (fromy - toy);
    // let projection = KMapUtils.scalarMultiplication(fromx, fromy, tox, toy, x, y);
    let projection = (tox - fromx) * (x - fromx) + (toy - fromy) * (y - fromy);
    let (prx, pry) = if projection < 0.0 {
        (fromx, fromy)
    } else if projection >= m_dist {
        (tox, toy)
    } else {
        (
            fromx + (tox - fromx) * (projection / m_dist),
            fromy + (toy - fromy) * (projection / m_dist),
        )
    };
    return ((prx - x) * (prx - x) + (pry - y) * (pry - y)).sqrt();
}

#[instrument(skip_all)]
fn approximate_elevation_points(points: &mut Vec<ElevationPoint>) -> eyre::Result<bool> {
    const SLOPE_THRESHOLD: f64 = 70.0;
    let mut last_survived = 0;
    let mut survived_count = 0;
    for i in 1..points.len() - 1 {
        let prev_ele = points
            .get(last_survived)
            .ok_or_eyre("Point not found")?
            .elevation;
        let ele = points.get(i).ok_or_eyre("Point not found")?.elevation;
        let ele_next = points.get(i + 1).ok_or_eyre("Point not found")?.elevation;
        if (ele - prev_ele) * (ele_next - ele) > 0.0 {
            points[i].survived = true;
            last_survived = i;
            survived_count += 1;
        }
    }
    points.last_mut().unwrap().survived = true;
    survived_count += 1;
    if survived_count < 2 {
        return Ok(false);
    }

    last_survived = 0;
    survived_count = 0;
    for i in 1..points.len() - 1 {
        if !points.get(i).ok_or_eyre("Point not found")?.survived {
            continue;
        }

        let ele = points.get(i).ok_or_eyre("Point not found")?.elevation;
        let prev_ele = points
            .get(last_survived)
            .ok_or_eyre("Point not found")?
            .elevation;
        let dist = Line::new(points[i].point, points[last_survived].point).length::<Haversine>();
        let slope = (ele - prev_ele) * 100.0 / dist;
        if slope.abs() > SLOPE_THRESHOLD {
            points[i].survived = false;
            continue;
        }
        last_survived = i;
        survived_count += 1;
    }
    if survived_count < 2 {
        return Ok(false);
    }

    points[0].survived = true;
    points.last_mut().unwrap().survived = true;
    let mut elevation_points = Vec::new();
    last_survived = 0;
    for i in 0..points.len() {
        if !points[i].survived {
            continue;
        }
        elevation_points.push(ElevationPoint {
            distance: if last_survived == 0 {
                0.0
            } else {
                Line::new(points[i].point, points[last_survived].point).length::<Haversine>()
            },
            elevation: points[i].elevation,
            point: points[i].point,
            extremum: false,
            survived: true,
        });
        last_survived = i;
    }
    *points = elevation_points;
    Ok(true)
}

#[instrument(skip(points))]
fn find_maximum_extremum_between(
    start: usize,
    end: usize,
    points: &mut Vec<ElevationPoint>,
) -> eyre::Result<()> {
    const ELE_THRESHOLD: f64 = 7.0;

    let first_point_dist = points.get(start).ok_or_eyre("Point not found")?.distance;
    let first_point_ele = points.get(start).ok_or_eyre("Point not found")?.elevation;
    let end_point_dist = points.get(end).ok_or_eyre("Point not found")?.distance;
    let end_point_ele = points.get(end).ok_or_eyre("Point not found")?.elevation;
    let mut max = start;
    let mut max_diff = ELE_THRESHOLD;
    for i in start + 1..end {
        let md = get_projection_dist(
            points.get(i).ok_or_eyre("Point not found")?.distance,
            points.get(i).ok_or_eyre("Point not found")?.elevation,
            first_point_dist,
            first_point_ele,
            end_point_dist,
            end_point_ele,
        );
        if md > max_diff {
            max = i;
            max_diff = md;
        }
    }
    if max != start {
        points[max].extremum = true;
        find_maximum_extremum_between(start, max, points)?;
        find_maximum_extremum_between(max, end, points)?;
    }
    Ok(())
}
