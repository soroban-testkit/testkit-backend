#!/usr/bin/env ruby
# frozen_string_literal: true

# Checks the metadata of every issue template in .github/ISSUE_TEMPLATE.
#
#   ruby .github/scripts/check-issue-templates.rb [template-dir]
#
# Issue forms (.yml / .yaml, except config.yml) need a non-empty `name` and
# `description`, an optional `labels` list, and a `body` of fields. Each field
# needs a known `type`, a non-empty `attributes.label` (markdown fields need
# `attributes.value` instead), and an `id` that is unique within the form.
# Dropdown and checkbox fields need a non-empty `attributes.options` list.
# Markdown templates (.md) need YAML front matter with `name` and `about`.
# config.yml may set `blank_issues_enabled` to true or false.
#
# Every problem is listed before the script exits, so one run shows them all.

require 'yaml'

FORM_FIELD_TYPES = %w[markdown textarea input dropdown checkboxes].freeze
OPTION_FIELD_TYPES = %w[dropdown checkboxes].freeze
ID_PATTERN = /\A[A-Za-z0-9_-]+\z/
INVALID = Object.new.freeze

def non_empty_string?(value)
  value.is_a?(String) && !value.strip.empty?
end

# Returns INVALID (and records the error) when the text is not valid YAML.
def load_yaml(text, path, errors)
  YAML.safe_load(text)
rescue Psych::Exception => e
  errors << "#{path}: invalid YAML (#{e.message.lines.first.to_s.strip})"
  INVALID
end

def check_issue_form(path, form, errors)
  return if form.equal?(INVALID)

  unless form.is_a?(Hash)
    errors << "#{path}: the file must be a YAML mapping with name, description and body"
    return
  end

  %w[name description].each do |key|
    errors << "#{path}: `#{key}` must be a non-empty string" unless non_empty_string?(form[key])
  end

  labels = form['labels']
  if form.key?('labels') && !(labels.is_a?(Array) && labels.all? { |label| non_empty_string?(label) })
    errors << "#{path}: `labels` must be a list of non-empty strings"
  end

  body = form['body']
  unless body.is_a?(Array) && !body.empty?
    errors << "#{path}: `body` must be a non-empty list of fields"
    return
  end

  seen_ids = []
  body.each_with_index do |field, index|
    where = "#{path}: body[#{index}]"
    unless field.is_a?(Hash) && FORM_FIELD_TYPES.include?(field['type'])
      errors << "#{where}: `type` must be one of #{FORM_FIELD_TYPES.join(', ')}"
      next
    end

    attributes = field['attributes']
    unless attributes.is_a?(Hash)
      errors << "#{where}: `attributes` must be a mapping"
      next
    end

    if field['type'] == 'markdown'
      errors << "#{where}: markdown fields need a non-empty `attributes.value`" unless non_empty_string?(attributes['value'])
      next
    end

    errors << "#{where}: `attributes.label` must be a non-empty string" unless non_empty_string?(attributes['label'])

    id = field['id']
    if !non_empty_string?(id) || id !~ ID_PATTERN
      errors << "#{where}: `id` must use only letters, digits, `-` or `_`"
    elsif seen_ids.include?(id)
      errors << "#{where}: duplicate id #{id.inspect}; ids must be unique within a form"
    else
      seen_ids << id
    end

    next unless OPTION_FIELD_TYPES.include?(field['type'])

    options = attributes['options']
    errors << "#{where}: #{field['type']} fields need a non-empty `attributes.options` list" unless options.is_a?(Array) && !options.empty?
  end
end

def check_markdown_template(path, errors)
  front_matter = File.read(path)[/\A---\n(.*?)\n---\n/m, 1]
  if front_matter.nil?
    errors << "#{path}: missing YAML front matter between --- lines at the top of the file"
    return
  end

  metadata = load_yaml(front_matter, path, errors)
  return if metadata.equal?(INVALID)

  unless metadata.is_a?(Hash)
    errors << "#{path}: front matter must be a YAML mapping with name and about"
    return
  end

  %w[name about].each do |key|
    errors << "#{path}: front matter `#{key}` must be a non-empty string" unless non_empty_string?(metadata[key])
  end

  return unless metadata.key?('labels')

  labels = metadata['labels']
  valid = labels.is_a?(String) || (labels.is_a?(Array) && labels.all? { |label| non_empty_string?(label) })
  errors << "#{path}: front matter `labels` must be a string or a list of non-empty strings" unless valid
end

def check_config(path, config, errors)
  return if config.equal?(INVALID)

  unless config.is_a?(Hash)
    errors << "#{path}: the file must be a YAML mapping"
    return
  end

  return unless config.key?('blank_issues_enabled')
  return if [true, false].include?(config['blank_issues_enabled'])

  errors << "#{path}: `blank_issues_enabled` must be true or false"
end

template_dir = ARGV.first || File.join(__dir__, '..', 'ISSUE_TEMPLATE')
templates = Dir.glob(File.join(template_dir, '*')).sort.select do |path|
  %w[.yml .yaml .md].include?(File.extname(path))
end

if templates.empty?
  abort "error: no issue templates found in #{template_dir}. Add an issue form (.yml) or a template (.md), or remove this check."
end

errors = []
templates.each do |path|
  if File.extname(path) == '.md'
    check_markdown_template(path, errors)
    next
  end

  data = load_yaml(File.read(path), path, errors)
  if File.basename(path, '.*') == 'config'
    check_config(path, data, errors)
  else
    check_issue_form(path, data, errors)
  end
end

if errors.empty?
  puts "OK: #{templates.size} issue template file(s) have valid metadata."
else
  errors.each { |error| warn "error: #{error}" }
  warn "Fix the file(s) above in #{template_dir}. Working examples are in .github/ISSUE_TEMPLATE."
  exit 1
end
